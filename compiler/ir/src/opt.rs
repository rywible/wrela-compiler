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
//! CPU code inlines small functions and those called from one place ([`inline_cpu`]).
//!
//! A function that takes no uniform data stays a call, when it's big (`outlined`): it takes
//! only scalars and vectors, so a call copies nothing slow, and its body appears once in the
//! WGSL rather than once per caller. That's what keeps nested derivations small (AC12): value
//! noise's derived versions are called from every octave, every gradient component and every
//! interval of a field.

use crate::single_exit::single_exit;
use crate::*;
use std::collections::{HashMap, HashSet};

/// Flattens a GPU module (see the module docs).
pub fn flatten_gpu(m: &mut Module) -> Result<()> {
    let keep = outlined(m);
    inline_all(m, &keep)?;
    // The calls left are to the kept functions, which take values: only a store writes a
    // local. The entry points and the kept functions they reach are kept (and optimized).
    keep_reachable(m, &keep);
    for i in 0..m.functions.len() {
        let mut f = std::mem::take(&mut m.functions[i]);
        forward(m, &mut f);
        promote(m, &mut f);
        dce(&mut f);
        // An inlined function given a constant `bool` keeps only the branch it takes (the lens's
        // `isolate: false`), as the CPU's inlining does.
        fold_constant_branches(&mut f);
        collapse_rebuilds(&mut f);
        cse(&mut f);
        dce(&mut f);
        m.functions[i] = f;
    }
    Ok(())
}

/// A value built of another's parts, all of them in order (`Construct(t, [x.0, x.1, ...])`
/// with `x` of type `t`), is that value: derived code rewraps its dual numbers so at each step,
/// and in WGSL each rewrap is a statement and a copy. (`x` is defined before its parts, so it's
/// visible wherever the rebuilt value is.)
fn collapse_rebuilds(f: &mut Function) {
    let mut extract: HashMap<ValueId, (ValueId, u32)> = HashMap::new();
    visit::walk(&f.body, &mut |s| {
        if let Stmt::Let(v, Expr::Extract(x, i)) = s {
            extract.insert(*v, (*x, *i));
        }
    });
    if extract.is_empty() {
        return;
    }
    let mut rename: Vec<Option<ValueId>> = vec![None; f.values.len()];
    visit::walk(&f.body, &mut |s| {
        if let Stmt::Let(w, Expr::Construct(t, parts)) = s
            && let Some(&(x, 0)) = parts.first().and_then(|p| extract.get(p))
            && f.values[x.index()] == *t
            && parts.iter().enumerate().all(|(i, p)| extract.get(p) == Some(&(x, i as u32)))
        {
            rename[w.index()] = Some(x);
        }
    });
    if !rename.iter().any(Option::is_some) {
        return;
    }
    // A rebuilt value of a rebuilt value is the first.
    let resolve = |mut v: ValueId| {
        while let Some(Some(y)) = rename.get(v.index()) {
            v = *y;
        }
        v
    };
    visit::walk_mut(&mut f.body, &mut |s| s.for_each_value_mut(&mut |x| *x = resolve(*x)));
}

// ---- common subexpressions -------------------------------------------------------------------

/// Gives a pure expression computed twice, where the first is visible, the first's value: the
/// same constant made again, or the same product of an interval's corners. Loads and calls
/// aren't merged, nor is anything with an effect.
fn cse(f: &mut Function) {
    let mut rename: Vec<Option<ValueId>> = vec![None; f.values.len()];
    let mut body = std::mem::take(&mut f.body);
    cse_block(&mut body, &mut rename, &mut HashMap::new());
    f.body = body;
}

fn cse_key(e: &Expr) -> Option<String> {
    match e {
        Expr::Const(_)
        | Expr::Zero(_)
        | Expr::Unary(..)
        | Expr::Binary(..)
        | Expr::Builtin(..)
        | Expr::Construct(..)
        | Expr::Extract(..)
        | Expr::ExtractDyn(..)
        | Expr::Splat(..)
        | Expr::Swizzle(..)
        | Expr::Convert(..)
        | Expr::Bitcast(..)
        | Expr::Select { .. } => Some(format!("{e:?}")),
        _ => None,
    }
}

/// `seen`: the expressions computed in `b` and in the blocks around it, which its statements can
/// use. What `b` adds goes again when it ends.
fn cse_block(b: &mut Block, rename: &mut [Option<ValueId>], seen: &mut HashMap<String, ValueId>) {
    let mut added = Vec::new();
    let old = std::mem::take(b);
    for mut s in old {
        s.for_each_value_mut(&mut |x| {
            if let Some(Some(y)) = rename.get(x.index()) {
                *x = *y;
            }
        });
        match s {
            Stmt::Let(v, ref e) if !e.has_effect() => match cse_key(e) {
                Some(k) => match seen.get(&k) {
                    Some(&w) => rename[v.index()] = Some(w),
                    None => {
                        seen.insert(k.clone(), v);
                        added.push(k);
                        b.push(s);
                    }
                },
                None => b.push(s),
            },
            Stmt::If { cond, mut then, mut else_ } => {
                cse_block(&mut then, rename, seen);
                cse_block(&mut else_, rename, seen);
                b.push(Stmt::If { cond, then, else_ });
            }
            Stmt::Loop { mut body, mut continuing } => {
                cse_block(&mut body, rename, seen);
                // `continuing` sees the body's top-level values, which the body's own merging
                // already renamed: what it adds isn't known here, so it starts from the outside.
                cse_block(&mut continuing, rename, seen);
                b.push(Stmt::Loop { body, continuing });
            }
            other => b.push(other),
        }
    }
    for k in added {
        seen.remove(&k);
    }
}

// ---- promoting `let` bindings to values ----------------------------------------------------

/// Replaces reads of locals written once, whole, before every read (a `let` binding's local)
/// with the value written, or a part of it: so a binding is a value, not a variable that's
/// stored and loaded at every use. (After `forward`, which leaves reads of read-only resources
/// going to the resource.) A read with an index that depends on a value stays a load.
fn promote(m: &Module, f: &mut Function) {
    let ok = forwardable_locals(f);
    if ok.is_empty() {
        return;
    }
    let mut stored: HashMap<LocalId, ValueId> = HashMap::new();
    visit::walk(&f.body, &mut |s| {
        if let Stmt::Store(p, v) = s
            && p.path.is_empty()
            && let Some(l) = p.root_local()
            && ok.contains(&l)
        {
            stored.insert(l, *v);
        }
    });
    let mut rename: Vec<Option<ValueId>> = vec![None; f.values.len()];
    let mut body = std::mem::take(&mut f.body);
    promote_block(m, f, &mut body, &stored, &mut rename, &mut HashSet::new());
    f.body = body;
}

/// `visible`: the values defined in `b` and in the blocks around it, which its statements can
/// use. What `b` adds goes again when it ends.
fn promote_block(
    m: &Module,
    f: &mut Function,
    b: &mut Block,
    stored: &HashMap<LocalId, ValueId>,
    rename: &mut Vec<Option<ValueId>>,
    visible: &mut HashSet<ValueId>,
) {
    let mut added = Vec::new();
    let old = std::mem::take(b);
    // A value passed down through inlined calls is a local read from a local read from a
    // local: a value stored is read as it's renamed, so one pass follows the whole chain.
    let renamed = |rename: &[Option<ValueId>], x: ValueId| {
        rename.get(x.index()).copied().flatten().unwrap_or(x)
    };
    for mut s in old {
        s.for_each_value_mut(&mut |x| *x = renamed(rename, *x));
        match s {
            Stmt::Let(v, Expr::Load(ref p))
                if p.root_local()
                    .and_then(|l| stored.get(&l))
                    .is_some_and(|&x| visible.contains(&renamed(rename, x)))
                    && p.path.iter().all(|x| !matches!(x, Proj::Index(_))) =>
            {
                let l = p.root_local().expect("a local");
                let mut at = renamed(rename, stored[&l]);
                if p.path.is_empty() {
                    rename[v.index()] = Some(at);
                    continue;
                }
                // Each projection of the path an extract of the value.
                for (k, proj) in p.path.iter().enumerate() {
                    let i = match proj {
                        Proj::Field(i) => *i,
                        Proj::Comp(c) => u32::from(*c),
                        Proj::Index(_) => unreachable!("checked above"),
                    };
                    let t = m.proj_ty(f.value_ty(at), proj).unwrap_or(f.value_ty(v));
                    let w = if k + 1 == p.path.len() { v } else { f.new_value(t) };
                    b.push(Stmt::Let(w, Expr::Extract(at, i)));
                    visible.insert(w);
                    added.push(w);
                    at = w;
                }
                if rename.len() < f.values.len() {
                    rename.resize(f.values.len(), None);
                }
            }
            Stmt::If { cond, mut then, mut else_ } => {
                promote_block(m, f, &mut then, stored, rename, visible);
                promote_block(m, f, &mut else_, stored, rename, visible);
                b.push(Stmt::If { cond, then, else_ });
            }
            Stmt::Loop { mut body, mut continuing } => {
                promote_block(m, f, &mut body, stored, rename, visible);
                // `continuing` sees what the body defines at its top level (WGSL's rule).
                let body_values: Vec<ValueId> = body
                    .iter()
                    .filter_map(|st| if let Stmt::Let(v, _) = st { Some(*v) } else { None })
                    .collect();
                visible.extend(&body_values);
                promote_block(m, f, &mut continuing, stored, rename, visible);
                for v in &body_values {
                    visible.remove(v);
                }
                b.push(Stmt::Loop { body, continuing });
            }
            other => {
                if let Stmt::Let(v, _) = &other {
                    visible.insert(*v);
                    added.push(*v);
                }
                b.push(other);
            }
        }
    }
    for v in added {
        visible.remove(&v);
    }
}

// ---- inlining -------------------------------------------------------------------------------

/// The smallest body, in statements once its own callees are inlined, that stays a call.
const OUTLINE_SIZE: usize = 48;

/// The size from which a function that takes few arguments stays a call (see `outlined`).
const OUTLINE_SMALL: usize = 24;

/// The most scalars a function's arguments (and its result) hold for it to stay a call by size
/// alone: a few registers.
const SMALL_ARGS: usize = 48;

/// How many scalars a value of type `t` holds, if a call passes it in registers: a scalar, a
/// vector, or a struct or enum of them, nested less than 8 deep. `None` for any other type: an
/// array passed by value is a private copy, which a GPU indexes slowly (why GPU code was all
/// inlined to begin with).
fn scalars(m: &Module, t: TypeId, depth: u32) -> Option<usize> {
    match m.types.get(t) {
        TypeDef::Scalar(_) => Some(1),
        TypeDef::Vector(n) => Some(*n as usize),
        TypeDef::Struct { fields, .. } if depth < 8 => fields
            .iter()
            .try_fold(0usize, |n, (_, ft)| Some(n.saturating_add(scalars(m, *ft, depth + 1)?))),
        TypeDef::Enum { variants, .. } if depth < 8 => {
            variants.iter().try_fold(1usize, |n, (_, p)| match p {
                Some(pt) => Some(n.saturating_add(scalars(m, *pt, depth + 1)?)),
                None => Some(n),
            })
        }
        _ => None,
    }
}

/// Callees first: an order in which every function comes after those it calls.
fn callee_order(m: &Module) -> Result<(Vec<usize>, Vec<bool>)> {
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
    Ok((order, called))
}

/// Which functions stay calls: big interval derivations that take and return only values a
/// call passes in registers (scalars, vectors, and structs of them without arrays), and whose
/// code, with
/// what it inlines, only computes: no entry point's inputs, resources, barriers, atomics or
/// textures. (Entry points are written whole.)
fn outlined(m: &Module) -> Vec<bool> {
    // Small arguments' functions stay calls only where the inlined shader would be too big
    // to write (a creature's, the lens's): elsewhere every call is inlined, as M1 measured
    // fastest (a branch-free kernel the GPU compiler sees whole).
    let (keep, biggest) = outlined_with(m, false);
    if biggest > INLINE_LIMIT { outlined_with(m, true).0 } else { keep }
}

/// The size of an entry point inlined whole, in statements, past which functions that take
/// few arguments stay calls. M1's fields are at most half of it; sketch 02's shaders and the
/// wolf's and the lens's are over (up to twelve times), and would be MBs of WGSL.
const INLINE_LIMIT: usize = 16000;

/// Which functions stay calls (see `outlined`), with small arguments' functions too if
/// `small_calls`; and the biggest entry point's size with what it inlines.
fn outlined_with(m: &Module, small_calls: bool) -> (Vec<bool>, usize) {
    let n = m.functions.len();
    let mut keep = vec![false; n];
    let Ok((order, _)) = callee_order(m) else { return (keep, 0) };
    let entry: HashSet<usize> = m.entry_points.iter().map(|e| e.function.index()).collect();
    // A body's size with its inlined callees, and whether it only computes.
    let mut size = vec![0usize; n];
    let mut pure = vec![false; n];
    for f in order {
        let func = &m.functions[f];
        let (mut sz, mut ok) = (0usize, true);
        visit::walk(&func.body, &mut |s| {
            sz += 1;
            if let Stmt::Store(p, _) = s
                && !matches!(p.root, PlaceRoot::Local(_))
            {
                ok = false;
            }
            if let Some(e) = s.expr() {
                match e {
                    Expr::Call(g, _) => {
                        if !keep[g.index()] {
                            sz += size[g.index()];
                            ok &= pure[g.index()];
                        }
                    }
                    Expr::EntryInput(_)
                    | Expr::Barrier
                    | Expr::Atomic(..)
                    | Expr::Texture(..)
                    | Expr::Host(..)
                    | Expr::Mem(..)
                    | Expr::Addr(_)
                    | Expr::Run(_)
                    | Expr::ArrayLength(_) => ok = false,
                    _ => {}
                }
                e.for_each_place(&mut |p| {
                    if !matches!(p.root, PlaceRoot::Local(_) | PlaceRoot::Data(_)) {
                        ok = false;
                    }
                });
            }
        });
        size[f] = sz;
        pure[f] = ok;
        // How many scalars the arguments and the result hold: `None` if one isn't passed in
        // registers (or is passed by reference).
        let args = func.params.iter().try_fold(0usize, |n, p| {
            if p.by_ref { None } else { Some(n.saturating_add(scalars(m, p.ty, 0)?)) }
        });
        let ret = func.ret.map_or(Some(0), |t| scalars(m, t, 0));
        let signature = args.is_some() && ret.is_some() && !func.ret_ref;
        // Interval derivations: they're what nested derivations call from many places. And
        // any other big function whose arguments are a few registers' worth (value noise, a
        // part's shape): its body appears once, where inlined it would appear at every use of
        // the field (its distance, its gradient, its parts). Other code stays inlined, as M1
        // measured it: a field's parts called in a loop (the grazer's legs) ran slower as
        // calls, each copying its share of the uniform.
        let small = args.is_some_and(|n| n <= SMALL_ARGS) && ret.is_some_and(|n| n <= SMALL_ARGS);
        let big = if func.interval {
            sz >= OUTLINE_SIZE
        } else {
            small_calls && small && sz >= OUTLINE_SMALL
        };
        keep[f] = !entry.contains(&f) && ok && signature && big;
    }
    let biggest = entry.iter().map(|&e| size[e]).max().unwrap_or(0);
    (keep, biggest)
}

/// Inlines every call but those to `keep`'s functions, callees first.
fn inline_all(m: &mut Module, keep: &[bool]) -> Result<()> {
    let (order, called) = callee_order(m)?;
    let n = m.functions.len();
    let mut exits: Vec<Option<Function>> = vec![None; n];
    for f in order {
        inline_into(m, f, &mut exits, keep, None)?;
        if called[f] {
            exits[f] = exit_form(m, f);
        }
    }
    Ok(())
}

/// `f`'s single-exit form, for inlining: `None` when its body has one exit already.
fn exit_form(m: &mut Module, f: usize) -> Option<Function> {
    let func = std::mem::take(&mut m.functions[f]);
    let form = single_exit(m, &func).map(|(nf, _)| nf);
    m.functions[f] = func;
    form
}

/// Inlines `f`'s calls to the functions `keep` doesn't hold for, whose bodies are flattened
/// already: `exits` has each one's single-exit form (`None`: its body has one exit already).
/// With a `budget`, a call is inlined only while the body stays that many statements or fewer.
fn inline_into(
    m: &mut Module,
    f: usize,
    exits: &mut [Option<Function>],
    keep: &[bool],
    budget: Option<(usize, &[usize])>,
) -> Result<()> {
    let mut func = std::mem::take(&mut m.functions[f]);
    let body = std::mem::take(&mut func.body);
    let rename = vec![None; func.values.len()];
    let size = budget.map_or(0, |_| size_of(&body));
    let mut cx = Inliner { m, exits, f: &mut func, rename, keep, budget, size, at: None };
    let body = cx.block(body);
    func.body = body?;
    m.functions[f] = func;
    Ok(())
}

/// A body's size: its statements, at every depth, but source locations.
fn size_of(b: &Block) -> usize {
    let mut n = 0;
    visit::walk(b, &mut |s| n += usize::from(!matches!(s, Stmt::At(_))));
    n
}

struct Inliner<'a> {
    m: &'a Module,
    exits: &'a [Option<Function>],
    f: &'a mut Function,
    /// Values of the function being flattened that now go by another name (a call's result),
    /// by `ValueId`.
    rename: Vec<Option<ValueId>>,
    /// The functions that stay calls.
    keep: &'a [bool],
    /// The most statements the body may grow to, and each function's size (see `inline_into`).
    budget: Option<(usize, &'a [usize])>,
    /// The body's size so far, with what's inlined into it.
    size: usize,
    /// The caller's last source location, which its code after an inlined body is at again.
    at: Option<wrela_diag::Span>,
}

impl Inliner<'_> {
    /// Whether a call to `g` is inlined: it's not kept, and it fits the budget (whose size it
    /// then takes).
    fn inlines(&mut self, g: FuncId) -> bool {
        if self.keep[g.index()] {
            return false;
        }
        match self.budget {
            None => true,
            Some((most, sizes)) => {
                let grown = self.size + sizes[g.index()];
                let fits = grown <= most;
                if fits {
                    self.size = grown;
                }
                fits
            }
        }
    }

    fn block(&mut self, b: Block) -> Result<Block> {
        let mut out = Vec::new();
        for mut s in b {
            let rename = &self.rename;
            s.for_each_value_mut(&mut |x| *x = rename[x.index()].unwrap_or(*x));
            match s {
                Stmt::Let(_, Expr::Call(g, _)) | Stmt::Eval(Expr::Call(g, _))
                    if !self.inlines(g) =>
                {
                    out.push(s);
                }
                Stmt::Let(v, Expr::Call(g, args)) => {
                    let ty = self.f.value_ty(v);
                    self.rename[v.index()] = self.inline(g, &args, Some(ty), &mut out)?;
                    out.extend(self.at.map(Stmt::At));
                }
                Stmt::Eval(Expr::Call(g, args)) => {
                    self.inline(g, &args, None, &mut out)?;
                    out.extend(self.at.map(Stmt::At));
                }
                Stmt::At(span) => {
                    self.at = Some(span);
                    out.push(s);
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
    /// `ty` is the result's type (a pointer, for a projection).
    fn inline(
        &mut self,
        g: FuncId,
        args: &[Arg],
        ty: Option<TypeId>,
        out: &mut Block,
    ) -> Result<Option<ValueId>> {
        let callee = self.exits[g.index()].as_ref().unwrap_or(&self.m.functions[g.index()]);
        let locals = callee.locals.iter().map(|l| self.f.new_local(l.name.clone(), l.ty)).collect();
        let values = vec![None; callee.values.len()];
        let mut cx = Copier { callee, caller: self.f, args, values, locals, result: None };
        out.extend(cx.block(&callee.body)?);
        // A body that never ends (it loops forever, or traps) has no `return` to take the
        // result from: what follows the call never runs, but needs a value.
        let result = match (cx.result, ty) {
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
            Expr::Atomic(_, p, xs) => {
                *p = self.place(p)?;
                for x in xs.iter_mut() {
                    *x = self.val(*x)?;
                }
            }
            // A call to a function that stays one.
            Expr::Call(_, args) => {
                for a in args.iter_mut() {
                    match a {
                        Arg::Value(x) => *x = self.val(*x)?,
                        Arg::Place(p) => *p = self.place(p)?,
                    }
                }
            }
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

// ---- inlining on the CPU --------------------------------------------------------------------

/// The size of a body, in statements once its own callees are inlined, that the CPU inlines at
/// each of its calls.
const CPU_INLINE_SIZE: usize = 40;

/// The most statements a CPU function grows to with what's inlined into it.
const CPU_BUDGET: usize = 4000;

/// Inlines calls on the CPU (AC12): calls to small functions, and the one call to a function
/// that only one place calls. A closure is a function of its own, called by an instance of the
/// function it's passed to that takes it alone; so a query that takes a closure, such as
/// `each_within`, becomes the loop a hand-written query is. The closure's body is in the loop,
/// and what it captures by reference is the caller's own locals again: an inlined callee's
/// by-reference parameter is its argument's place, so a captured scalar's address isn't taken
/// and it stays in a WASM local. (Neither wasmtime nor a browser's compiler inlines across
/// WASM functions dependably.)
///
/// Recursive functions stay calls, as do counted ones (each call counts depth, language.md
/// §8). Inlined code keeps its source locations, so a trap's location is the same; a stack
/// trace no longer shows the calls that were inlined. The functions nothing reaches any more
/// are removed.
pub fn inline_cpu(m: &mut Module) -> Result<()> {
    for f in &mut m.functions {
        fold_constant_branches(f);
    }
    let n = m.functions.len();
    let callees: Vec<Vec<FuncId>> = m.functions.iter().map(|f| visit::calls(&f.body)).collect();
    let mut sites = vec![0u32; n];
    for g in callees.iter().flatten() {
        sites[g.index()] += 1;
    }
    let mut root = vec![false; n];
    for f in roots(m) {
        root[f] = true;
    }
    let recursive = recursive(&callees);
    let mut keep = vec![true; n];
    let mut sizes = vec![0usize; n];
    let mut exits: Vec<Option<Function>> = vec![None; n];
    for f in postorder(&callees) {
        inline_into(m, f, &mut exits, &keep, Some((CPU_BUDGET, &sizes)))?;
        sizes[f] = size_of(&m.functions[f].body);
        let once = sites[f] == 1 && !root[f];
        if !recursive[f] && !m.functions[f].counted && (sizes[f] <= CPU_INLINE_SIZE || once) {
            keep[f] = false;
            exits[f] = exit_form(m, f);
        }
    }
    remove_unreached(m);
    for i in 0..m.functions.len() {
        let mut f = std::mem::take(&mut m.functions[i]);
        reuse_loads(m, &mut f);
        m.functions[i] = f;
    }
    Ok(())
}

// ---- branches on constants (CPU) ------------------------------------------------------------

/// Replaces each `if` whose condition is a constant `bool` with the branch it takes. Code
/// behind `if debug_build()` is then not in a release build, and neither are the functions
/// only it calls (a failed `assert`'s formatted values, std's checks of declared bounds).
fn fold_constant_branches(f: &mut Function) {
    let body = std::mem::take(&mut f.body);
    f.body = fold_block(body, &mut HashMap::new());
}

fn fold_block(b: Block, known: &mut HashMap<ValueId, bool>) -> Block {
    let mut out = Vec::with_capacity(b.len());
    for s in b {
        match s {
            Stmt::Let(v, Expr::Const(Const::Bool(c))) => {
                known.insert(v, c);
                out.push(Stmt::Let(v, Expr::Const(Const::Bool(c))));
            }
            Stmt::Let(v, Expr::Unary(UnOp::Not, x)) if known.contains_key(&x) => {
                let c = !known[&x];
                known.insert(v, c);
                out.push(Stmt::Let(v, Expr::Const(Const::Bool(c))));
            }
            Stmt::If { cond, then, else_ } => match known.get(&cond).copied() {
                Some(c) => out.extend(fold_block(if c { then } else { else_ }, known)),
                None => out.push(Stmt::If {
                    cond,
                    then: fold_block(then, known),
                    else_: fold_block(else_, known),
                }),
            },
            Stmt::Loop { body, continuing } => out.push(Stmt::Loop {
                body: fold_block(body, known),
                continuing: fold_block(continuing, known),
            }),
            s => out.push(s),
        }
        // What follows a branch that always leaves the block never runs.
        if matches!(out.last(), Some(Stmt::Return(_) | Stmt::Break | Stmt::Continue | Stmt::Trap)) {
            break;
        }
    }
    out
}

// ---- loads that read what's read already (CPU) ----------------------------------------------

/// Gives a load of a place that was loaded or stored already, where nothing since could have
/// changed it, the value loaded or stored; then drops the stores to locals nothing reads. An inlined
/// closure's argument is often an element its caller just read (AC12): `f(points[i])` after
/// `points[i] - p`. The load is a bounds check and a copy, and the closure's copy of it a store.
///
/// A store to a local's own storage, where the local's address isn't taken, changes only
/// loads from that local. Any other write (through a pointer, a by-reference parameter or a
/// run; a call; a host or memory operation) may change every other load.
fn reuse_loads(m: &Module, f: &mut Function) {
    let mut taken = vec![false; f.locals.len()];
    visit::walk(&f.body, &mut |s| {
        let mut mark = |p: &Place| {
            if let Some(l) = p.root_local() {
                taken[l.index()] = true;
            }
        };
        match s.expr() {
            Some(Expr::Addr(p) | Expr::Run(p) | Expr::Atomic(_, p, _)) => mark(p),
            Some(Expr::Call(_, args)) => args.iter().for_each(|a| {
                if let Arg::Place(p) = a {
                    mark(p);
                }
            }),
            _ => {}
        }
    });
    let mut cx = Reuse { m, f, taken, rename: Vec::new() };
    cx.rename = vec![None; cx.f.values.len()];
    let mut body = std::mem::take(&mut cx.f.body);
    cx.block(&mut body, &mut HashMap::new());
    cx.f.body = body;
    drop_dead_stores(m, f);
}

/// A load whose value is known: its value, and whether its place is in a local's own storage
/// (and which local), rather than in memory that other writes can reach.
type Loaded = HashMap<Place, (ValueId, Option<LocalId>)>;

struct Reuse<'a> {
    m: &'a Module,
    f: &'a mut Function,
    /// The locals whose address is taken: their storage is memory that other writes can reach.
    taken: Vec<bool>,
    rename: Vec<Option<ValueId>>,
}

impl Reuse<'_> {
    /// The local whose own storage `p` is in, if it is: the path doesn't go through a run or a
    /// pointer.
    fn own(&self, p: &Place) -> Option<LocalId> {
        self.m.local_storage(self.f, p).filter(|l| !self.taken[l.index()])
    }

    /// What a statement's writes make unknown (not counting the blocks in it).
    fn forget(&self, s: &Stmt, known: &mut Loaded) {
        let everything_else = |known: &mut Loaded| known.retain(|_, (_, own)| own.is_some());
        match s {
            Stmt::Store(p, _) => match self.own(p) {
                Some(l) => known.retain(|q, _| q.root_local() != Some(l)),
                None => everything_else(known),
            },
            Stmt::Let(_, e) | Stmt::Eval(e) => {
                // A memory operation that only reads writes nothing.
                let reads = matches!(
                    e,
                    Expr::Mem(
                        MemOp::HeapBase
                            | MemOp::Pages
                            | MemOp::Load(_)
                            | MemOp::Ptr
                            | MemOp::Addr
                            | MemOp::ThreadBlock,
                        _
                    )
                );
                if e.has_effect() && !reads {
                    everything_else(known);
                }
            }
            _ => {}
        }
    }

    /// What `b` and the blocks in it make unknown, wherever it ends.
    fn forget_all(&self, b: &Block, known: &mut Loaded) {
        visit::walk(b, &mut |s| self.forget(s, known));
    }

    fn block(&mut self, b: &mut Block, known: &mut Loaded) {
        let old = std::mem::take(b);
        for mut s in old {
            let rename = &self.rename;
            s.for_each_value_mut(&mut |x| *x = rename[x.index()].unwrap_or(*x));
            match s {
                Stmt::Let(v, Expr::Load(p)) => {
                    if let Some(&(was, _)) = known.get(&p) {
                        self.rename[v.index()] = Some(was);
                        continue;
                    }
                    let own = self.own(&p);
                    known.insert(p.clone(), (v, own));
                    b.push(Stmt::Let(v, Expr::Load(p)));
                }
                Stmt::If { cond, mut then, mut else_ } => {
                    let (mut a, mut c) = (known.clone(), known.clone());
                    self.block(&mut then, &mut a);
                    self.block(&mut else_, &mut c);
                    known.retain(|p, x| a.get(p) == Some(&*x) && c.get(p) == Some(&*x));
                    b.push(Stmt::If { cond, then, else_ });
                }
                Stmt::Loop { mut body, mut continuing } => {
                    // What's known at the top of every iteration: what the loop never changes.
                    self.forget_all(&body, known);
                    self.forget_all(&continuing, known);
                    self.block(&mut body, &mut known.clone());
                    self.block(&mut continuing, &mut known.clone());
                    b.push(Stmt::Loop { body, continuing });
                }
                Stmt::Store(p, x) => {
                    // What's stored in a local's own storage is what a load of it reads next.
                    let s = Stmt::Store(p.clone(), x);
                    self.forget(&s, known);
                    if let Some(l) = self.own(&p) {
                        known.insert(p, (x, Some(l)));
                    }
                    b.push(s);
                }
                s => {
                    self.forget(&s, known);
                    b.push(s);
                }
            }
        }
    }
}

/// Drops the stores to locals that nothing reads (and whose place has no index, which could
/// trap). A store through a run or a pointer a local holds reads the local.
fn drop_dead_stores(m: &Module, f: &mut Function) {
    let mut read = vec![false; f.locals.len()];
    let through = |p: &Place| p.root_local().is_some() && m.local_storage(f, p).is_none();
    visit::walk(&f.body, &mut |s| {
        let mut mark = |p: &Place| {
            if let Some(l) = p.root_local() {
                read[l.index()] = true;
            }
        };
        match s {
            Stmt::Store(p, _) if through(p) => mark(p),
            Stmt::Store(..) => {}
            s => {
                if let Some(e) = s.expr() {
                    e.for_each_place(&mut mark);
                }
            }
        }
    });
    fn sweep(b: &mut Block, read: &[bool]) {
        b.retain_mut(|s| {
            for inner in s.blocks_mut() {
                sweep(inner, read);
            }
            !matches!(s, Stmt::Store(p, _)
                if p.root_local().is_some_and(|l| !read[l.index()])
                    && p.path.iter().all(|x| !matches!(x, Proj::Index(_))))
        });
    }
    sweep(&mut f.body, &read);
}

/// The functions a CPU module's code is entered at: its exports, its tasks and its thread
/// entries.
fn roots(m: &Module) -> Vec<usize> {
    let exports = m.exports.iter().map(|e| e.1);
    let entries = m.thread_entries.iter().map(|e| e.1);
    exports.chain(m.tasks.iter().copied()).chain(entries).map(|f| f.index()).collect()
}

/// An order of the functions in which each comes after those it calls, but where calls make a
/// cycle (each function in a cycle comes after the callees outside it).
fn postorder(callees: &[Vec<FuncId>]) -> Vec<usize> {
    fn visit(f: usize, callees: &[Vec<FuncId>], state: &mut [u8], order: &mut Vec<usize>) {
        if state[f] != 0 {
            return;
        }
        state[f] = 1;
        for g in &callees[f] {
            visit(g.index(), callees, state, order);
        }
        state[f] = 2;
        order.push(f);
    }
    let mut state = vec![0u8; callees.len()];
    let mut order = Vec::with_capacity(callees.len());
    for f in 0..callees.len() {
        visit(f, callees, &mut state, &mut order);
    }
    order
}

/// Which functions are in a cycle of calls: Tarjan's strongly connected components.
fn recursive(callees: &[Vec<FuncId>]) -> Vec<bool> {
    struct Tarjan<'a> {
        callees: &'a [Vec<FuncId>],
        index: Vec<u32>,
        low: Vec<u32>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        next: u32,
        cyclic: Vec<bool>,
    }
    impl Tarjan<'_> {
        fn visit(&mut self, f: usize) {
            (self.index[f], self.low[f]) = (self.next, self.next);
            self.next += 1;
            self.stack.push(f);
            self.on_stack[f] = true;
            let callees = self.callees;
            for g in callees[f].iter().map(|g| g.index()) {
                if self.index[g] == u32::MAX {
                    self.visit(g);
                    self.low[f] = self.low[f].min(self.low[g]);
                } else if self.on_stack[g] {
                    self.low[f] = self.low[f].min(self.index[g]);
                }
            }
            if self.low[f] == self.index[f] {
                let mut component = Vec::new();
                while let Some(g) = self.stack.pop() {
                    self.on_stack[g] = false;
                    component.push(g);
                    if g == f {
                        break;
                    }
                }
                let cyclic = component.len() > 1 || callees[f].iter().any(|g| g.index() == f);
                for g in component {
                    self.cyclic[g] = cyclic;
                }
            }
        }
    }
    let n = callees.len();
    let mut t = Tarjan {
        callees,
        index: vec![u32::MAX; n],
        low: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        next: 0,
        cyclic: vec![false; n],
    };
    for f in 0..n {
        if t.index[f] == u32::MAX {
            t.visit(f);
        }
    }
    t.cyclic
}

/// Removes the CPU functions that nothing reaches from the module's roots, and renumbers the
/// rest.
fn remove_unreached(m: &mut Module) {
    let remap = retain_reached(m, roots(m));
    let renumber = |f: &mut FuncId| *f = FuncId(remap[f.index()].expect("a reached function"));
    m.exports.iter_mut().for_each(|e| renumber(&mut e.1));
    m.tasks.iter_mut().for_each(renumber);
    m.thread_entries.iter_mut().for_each(|e| renumber(&mut e.1));
}

/// Removes the functions that nothing reaches from `roots`, and renumbers the calls in the
/// rest: each function's new index, by its old one.
fn retain_reached(m: &mut Module, roots: Vec<usize>) -> Vec<Option<u32>> {
    let mut reached = vec![false; m.functions.len()];
    let mut work = roots;
    while let Some(f) = work.pop() {
        if !std::mem::replace(&mut reached[f], true) {
            work.extend(visit::calls(&m.functions[f].body).iter().map(|g| g.index()));
        }
    }
    let remap = retain_by_index(&mut m.functions, |i| reached[i]);
    for f in &mut m.functions {
        visit::walk_mut(&mut f.body, &mut |s| {
            if let Stmt::Let(_, Expr::Call(g, _)) | Stmt::Eval(Expr::Call(g, _)) = s {
                *g = FuncId(remap[g.index()].expect("a callee is reached"));
            }
        });
    }
    remap
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
        Stmt::Let(v, e) if !e.has_effect() => {
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
            Stmt::Let(v, e) => used[v.index()] || e.has_effect(),
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
/// Keeps the entry points and the outlined functions they call, and renumbers the calls.
fn keep_reachable(m: &mut Module, outlined: &[bool]) {
    debug_assert!(m.exports.is_empty(), "a GPU module exports nothing");
    debug_assert!(
        m.functions.iter().flat_map(|f| visit::calls(&f.body)).all(|g| outlined[g.index()]),
        "only outlined functions stay calls"
    );
    let roots = m.entry_points.iter().map(|e| e.function.index()).collect();
    let remap = retain_reached(m, roots);
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

    fn param(m: &mut Module, by_ref: bool) -> Param {
        Param { name: "p".into(), ty: m.types.u32(), by_ref, mutable: by_ref }
    }

    fn loads(f: &Function) -> usize {
        let mut n = 0;
        visit::walk(&f.body, &mut |s| n += usize::from(matches!(s, Stmt::Let(_, Expr::Load(_)))));
        n
    }

    /// A load of what was loaded or stored already is that value, unless something between
    /// could write the place: a call, for a by-reference parameter's place; a store in a loop,
    /// for a load before it. A local's own storage, its address not taken, survives a call.
    #[test]
    fn a_load_is_reused_until_something_could_change_it() {
        let mut m = Module::default();
        let u32t = m.types.u32();
        let g = m.add_function(Function::new("g", Vec::new(), None));
        let p = param(&mut m, true);
        let mut f = Function::new("f", vec![p], Some(u32t));
        let l = f.new_local("l", u32t);
        let vs: Vec<ValueId> = (0..6).map(|_| f.new_value(u32t)).collect();
        let at_p = Place::root(PlaceRoot::Param(0));
        f.body = vec![
            Stmt::Let(vs[0], Expr::Load(at_p.clone())),
            Stmt::Store(Place::local(l), vs[0]),
            Stmt::Let(vs[1], Expr::Load(Place::local(l))),
            Stmt::Let(vs[2], Expr::Load(at_p.clone())), // reused
            Stmt::Eval(Expr::Call(g, Vec::new())),
            Stmt::Let(vs[3], Expr::Load(at_p)), // a call came between
            Stmt::Let(vs[4], Expr::Load(Place::local(l))), // stored: l's address isn't taken
            Stmt::Loop {
                body: vec![
                    Stmt::Let(vs[5], Expr::Load(Place::local(l))), // the loop stores l
                    Stmt::Store(Place::local(l), vs[5]),
                    Stmt::Break,
                ],
                continuing: Vec::new(),
            },
            Stmt::Return(Some(vs[4])),
        ];
        reuse_loads(&m, &mut f);
        assert_eq!(loads(&f), 3, "{:?}", f.body);
        assert_eq!(f.body.last(), Some(&Stmt::Return(Some(vs[0]))));
    }

    /// A store through a run a local holds reads the local: the store that set the local
    /// stays, while a store to a local nothing reads goes.
    #[test]
    fn a_store_through_a_run_keeps_the_run() {
        let mut m = Module::default();
        let f32t = m.types.f32();
        let u32t = m.types.u32();
        let arr_t = m.types.intern(TypeDef::Array(f32t, 4));
        let run_t = m.types.intern(TypeDef::Run(f32t));
        let mut f = Function::new("f", Vec::new(), None);
        let (arr, view, dead) =
            (f.new_local("arr", arr_t), f.new_local("view", run_t), f.new_local("dead", f32t));
        let (z, r, i, x) =
            (f.new_value(arr_t), f.new_value(run_t), f.new_value(u32t), f.new_value(f32t));
        f.body = vec![
            Stmt::Let(z, Expr::Zero(arr_t)),
            Stmt::Store(Place::local(arr), z),
            Stmt::Let(r, Expr::Run(Place::local(arr))),
            Stmt::Store(Place::local(view), r),
            Stmt::Let(i, Expr::Const(Const::U32(1))),
            Stmt::Let(x, Expr::Const(Const::F32(2.0))),
            Stmt::Store(Place::local(view).with(Proj::Index(i)), x),
            Stmt::Store(Place::local(dead), x),
            Stmt::Return(None),
        ];
        reuse_loads(&m, &mut f);
        let stores = |l: LocalId| {
            let mut n = 0;
            visit::walk(&f.body, &mut |s| {
                n += usize::from(matches!(s, Stmt::Store(p, _) if p.root_local() == Some(l)));
            });
            n
        };
        assert_eq!((stores(view), stores(dead)), (2, 0), "{:?}", f.body);
    }

    /// Small callees are inlined, and go when nothing else calls them; a recursive function
    /// stays a call, and an inlined by-reference parameter is its argument's place.
    #[test]
    fn the_cpu_inlines_small_calls_but_not_recursive_ones() {
        let mut m = Module::default();
        let u32t = m.types.u32();
        // fn inc(&mut x) { x = x + 1 }
        let p = param(&mut m, true);
        let mut inc = Function::new("inc", vec![p], None);
        let (a, one, b) = (inc.new_value(u32t), inc.new_value(u32t), inc.new_value(u32t));
        inc.body = vec![
            Stmt::Let(a, Expr::Load(Place::root(PlaceRoot::Param(0)))),
            Stmt::Let(one, Expr::Const(Const::U32(1))),
            Stmt::Let(b, Expr::Binary(BinOp::Add, a, one)),
            Stmt::Store(Place::root(PlaceRoot::Param(0)), b),
            Stmt::Return(None),
        ];
        let inc = m.add_function(inc);
        // fn rec() { rec() }
        let mut rec = Function::new("rec", Vec::new(), None);
        rec.body = vec![Stmt::Eval(Expr::Call(FuncId(1), Vec::new())), Stmt::Return(None)];
        let rec = m.add_function(rec);
        assert_eq!(rec, FuncId(1));
        // export fn main() -> u32 { var n = 0; inc(mut n); rec(); n }
        let mut main = Function::new("main", Vec::new(), Some(u32t));
        let n = main.new_local("n", u32t);
        let (zero, out) = (main.new_value(u32t), main.new_value(u32t));
        main.body = vec![
            Stmt::Let(zero, Expr::Const(Const::U32(0))),
            Stmt::Store(Place::local(n), zero),
            Stmt::Eval(Expr::Call(inc, vec![Arg::Place(Place::local(n))])),
            Stmt::Eval(Expr::Call(rec, Vec::new())),
            Stmt::Let(out, Expr::Load(Place::local(n))),
            Stmt::Return(Some(out)),
        ];
        let main = m.add_function(main);
        m.exports.push(("main".into(), main));
        inline_cpu(&mut m).expect("inline");
        let names: Vec<&str> = m.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["rec", "main"]);
        let main = &m.functions[1];
        assert_eq!(visit::calls(&main.body), [FuncId(0)]);
        // `inc` wrote `n` itself: so `n` is the sum, which is returned, and isn't stored.
        let mut sum = None;
        visit::walk(&main.body, &mut |s| {
            if let Stmt::Let(v, Expr::Binary(BinOp::Add, ..)) = s {
                sum = Some(*v);
            }
        });
        assert!(sum.is_some() && main.body.last() == Some(&Stmt::Return(sum)), "{:?}", main.body);
        let stores_n = |s: &Stmt| matches!(s, Stmt::Store(p, _) if p.root_local() == Some(n));
        assert!(!visit::any(&main.body, &mut |s| stores_n(s)));
        crate::verify(&m).expect("a valid module");
    }
}
