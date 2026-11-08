//! How often a GPU value is computed against how often it changes (language.md §18.13): the
//! values a function computes that are the same for every invocation of a command.
//!
//! A shader runs once a pixel (a kernel once an invocation), but some of what it computes
//! depends only on what the command gives every invocation alike: its uniform block, the
//! pipeline's constants, a read-only buffer read at the same index. Each invocation computes
//! that again, to the same result; the CPU could compute it once a frame and pass it in. A value
//! here is *same* when every invocation that computes it gets the same result, and *varying*
//! otherwise.
//!
//! A value is same when it's computed only from same values: constants, the uniform block, a
//! read-only buffer or a texture at a same place, a parameter the caller passes a same value,
//! and the result of a call given only same values (the functions GPU code still calls after
//! flattening are pure). A builtin input, a writable buffer, workgroup memory, an atomic, a
//! subgroup operation and a derivative vary. A local is same when every store to it stores a
//! same value, under control flow that every invocation takes alike: a store in a branch on a
//! varying condition, or in a loop that a varying condition leaves, makes it vary.

use crate::*;
use std::collections::HashSet;

/// The values of `f` that are the same for every invocation, given which of its parameters
/// are (`args[i]`; a parameter not given varies).
pub fn same_values(m: &Module, f: &Function, args: &[bool]) -> HashSet<ValueId> {
    analyse(m, f, args).0
}

/// The same values, and the locals that vary.
fn analyse(m: &Module, f: &Function, args: &[bool]) -> (HashSet<ValueId>, HashSet<LocalId>) {
    let mut a = Analysis {
        m,
        f,
        args,
        same: HashSet::new(),
        varying_locals: HashSet::new(),
        changed: true,
    };
    // Optimistic about locals: each round finds more that vary, until a round finds none.
    // Values are recomputed from scratch each round, so a value is same only if it is under
    // the final set of varying locals.
    while a.changed {
        a.changed = false;
        a.same.clear();
        a.block(&f.body, false);
    }
    (a.same, a.varying_locals)
}

struct Analysis<'a> {
    m: &'a Module,
    f: &'a Function,
    args: &'a [bool],
    same: HashSet<ValueId>,
    varying_locals: HashSet<LocalId>,
    changed: bool,
}

impl Analysis<'_> {
    fn vary(&mut self, l: LocalId) {
        if self.varying_locals.insert(l) {
            self.changed = true;
        }
    }

    /// `varying`: the block runs under control flow that invocations may take differently.
    fn block(&mut self, b: &[Stmt], varying: bool) {
        for s in b {
            match s {
                Stmt::Let(v, e) => {
                    if self.expr(e) {
                        self.same.insert(*v);
                    }
                    self.lent(e);
                }
                Stmt::Eval(e) => self.lent(e),
                Stmt::Store(p, v) => {
                    if let PlaceRoot::Local(l) = p.root
                        && (varying || !self.same.contains(v) || !self.place_same(p))
                    {
                        self.vary(l);
                    }
                }
                Stmt::If { cond, then, else_ } => {
                    let inner = varying || !self.same.contains(cond);
                    self.block(then, inner);
                    self.block(else_, inner);
                }
                Stmt::Loop { body, continuing } => {
                    // The loop's passes differ between invocations if a varying condition can
                    // leave it (or skip to its next pass).
                    // The conditions are defined in the loop, so it's walked first to know them.
                    self.block(body, varying);
                    self.block(continuing, varying);
                    if !varying && self.leaves_varying(body) {
                        self.block(body, true);
                        self.block(continuing, true);
                    }
                }
                Stmt::Break | Stmt::Continue | Stmt::Return(_) | Stmt::Trap | Stmt::At(_) => {}
            }
        }
    }

    /// Whether a `break`, `continue` or `return` in `b` (not in a loop nested in it) is under a
    /// branch on a condition that may vary.
    fn leaves_varying(&self, b: &[Stmt]) -> bool {
        fn leaves(b: &[Stmt]) -> bool {
            b.iter().any(|s| match s {
                Stmt::Break | Stmt::Continue | Stmt::Return(_) => true,
                Stmt::If { then, else_, .. } => leaves(then) || leaves(else_),
                // An inner loop's `break` and `continue` are its own; a `return` leaves this one.
                Stmt::Loop { body, continuing } => returns(body) || returns(continuing),
                _ => false,
            })
        }
        fn returns(b: &[Stmt]) -> bool {
            let mut hit = false;
            visit::walk(b, &mut |s| hit |= matches!(s, Stmt::Return(_)));
            hit
        }
        b.iter().any(|s| match s {
            Stmt::If { cond, then, else_ } => {
                if self.same.contains(cond) {
                    self.leaves_varying(then) || self.leaves_varying(else_)
                } else {
                    leaves(then) || leaves(else_)
                }
            }
            Stmt::Loop { body, continuing } => returns(body) || returns(continuing),
            _ => false,
        })
    }

    /// A place lent to a call may be written there.
    fn lent(&mut self, e: &Expr) {
        if let Expr::Call(_, args) = e {
            for a in args {
                if let Arg::Place(p) = a
                    && let PlaceRoot::Local(l) = p.root
                {
                    self.vary(l);
                }
            }
        }
    }

    /// Whether the place's element indices are all same.
    fn place_same(&self, p: &Place) -> bool {
        p.path.iter().all(|x| match x {
            Proj::Index(v) => self.same.contains(v),
            Proj::Field(_) | Proj::Comp(_) => true,
        })
    }

    fn all_same(&self, e: &Expr) -> bool {
        let mut all = true;
        e.for_each_value(&mut |v| all &= self.same.contains(&v));
        all
    }

    /// Whether `e`'s value is the same for every invocation.
    fn expr(&self, e: &Expr) -> bool {
        match e {
            Expr::Const(_) | Expr::Zero(_) => true,
            Expr::Param(i) => self.args.get(*i as usize).copied().unwrap_or(false),
            Expr::EntryInput(i) => {
                // Only the number of workgroups is the dispatch's, not the invocation's.
                let entry = self
                    .m
                    .entry_points
                    .iter()
                    .find(|ep| std::ptr::eq(&self.m.functions[ep.function.0 as usize], self.f));
                entry.and_then(|ep| ep.inputs.get(*i as usize))
                    == Some(&BuiltinInput::NumWorkgroups)
            }
            Expr::Load(p) => {
                let root = match p.root {
                    PlaceRoot::Local(l) => !self.varying_locals.contains(&l),
                    PlaceRoot::Resource(r) => matches!(
                        self.m.resources[r.0 as usize].kind,
                        ResourceKind::Uniform { .. } | ResourceKind::StorageRead
                    ),
                    PlaceRoot::Param(_) | PlaceRoot::Ptr(_) | PlaceRoot::Data(_) => false,
                };
                root && self.place_same(p)
            }
            Expr::ArrayLength(_) => true,
            Expr::Unary(..)
            | Expr::Binary(..)
            | Expr::Construct(..)
            | Expr::Variant(..)
            | Expr::Extract(..)
            | Expr::ExtractDyn(..)
            | Expr::Splat(..)
            | Expr::Swizzle(..)
            | Expr::Convert(..)
            | Expr::Bitcast(..)
            | Expr::Select { .. } => self.all_same(e),
            Expr::Builtin(b, _) => {
                !matches!(b, Builtin::Dpdx | Builtin::Dpdy | Builtin::Fwidth) && self.all_same(e)
            }
            // A sample takes its detail from the pixels around it, which a same coordinate
            // makes the same too.
            Expr::Texture(..) => self.all_same(e),
            Expr::Call(_, args) => {
                args.iter().all(|a| matches!(a, Arg::Value(v) if self.same.contains(v)))
            }
            Expr::Run(_)
            | Expr::Addr(_)
            | Expr::Host(..)
            | Expr::Mem(..)
            | Expr::TextureStore(..)
            | Expr::Atomic(..)
            | Expr::Subgroup(..)
            | Expr::Barrier
            | Expr::Discard => false,
        }
    }
}

/// Work every invocation does to compute the same value (§18.13): the value, where it's
/// computed, and its work, all that's same of what it's computed from included (`None`:
/// unbounded).
#[derive(Clone, Debug, PartialEq)]
pub struct SameWork {
    /// The function it's in, by name.
    pub function: String,
    pub at: Option<wrela_diag::Span>,
    pub work: Option<u64>,
}

/// The work each entry point of `m` does more than once to compute a value the same for every
/// invocation, at each place a varying computation reads one, of at least `least` work, most
/// first. A function that entry points reach with the same parameters same is looked at once.
pub fn same_work(m: &Module, least: u64) -> Vec<SameWork> {
    let mut s = Sites {
        m,
        work: crate::work::ExprWork::new(m),
        seen: HashSet::new(),
        out: Vec::new(),
        least,
    };
    for e in &m.entry_points {
        // A kernel of one invocation a workgroup is how GPU code runs once: what it computes,
        // it computes once a workgroup.
        if matches!(e.stage, Stage::Compute { workgroup_size: [1, 1, 1] }) {
            continue;
        }
        s.function(e.function, Vec::new());
    }
    let mut out = s.out;
    out.sort_by(|a, b| {
        let w = |x: &SameWork| x.work.unwrap_or(u64::MAX);
        w(b).cmp(&w(a)).then(a.function.cmp(&b.function))
    });
    out
}

struct Sites<'a> {
    m: &'a Module,
    work: crate::work::ExprWork<'a>,
    seen: HashSet<(u32, Vec<bool>)>,
    out: Vec<SameWork>,
    least: u64,
}

impl Sites<'_> {
    fn function(&mut self, id: FuncId, args: Vec<bool>) {
        if !self.seen.insert((id.0, args.clone())) {
            return;
        }
        let m = self.m;
        let f = &m.functions[id.0 as usize];
        let (same, varying) = analyse(m, f, &args);
        // Each same value's expression and where it is; what's stored to each local.
        let mut def: std::collections::HashMap<ValueId, (&Expr, Option<wrela_diag::Span>)> =
            std::collections::HashMap::new();
        let mut stored: std::collections::HashMap<LocalId, Vec<ValueId>> =
            std::collections::HashMap::new();
        // The same values a varying computation reads, where it reads them.
        let mut read: Vec<ValueId> = Vec::new();
        let mut calls: Vec<(FuncId, Vec<bool>)> = Vec::new();
        let mut here = None;
        visit::walk(&f.body, &mut |s| {
            let mut reads = |v: ValueId| {
                if same.contains(&v) {
                    read.push(v);
                }
            };
            match s {
                Stmt::At(span) => here = Some(*span),
                Stmt::Let(v, e) if same.contains(v) => {
                    def.insert(*v, (e, here));
                }
                Stmt::Let(_, e) | Stmt::Eval(e) => {
                    e.for_each_value(&mut reads);
                    if let Expr::Call(g, a) = e {
                        let bits = a
                            .iter()
                            .map(|a| matches!(a, Arg::Value(v) if same.contains(v)))
                            .collect();
                        calls.push((*g, bits));
                    }
                }
                Stmt::Store(p, v) => match p.root {
                    PlaceRoot::Local(l) if !varying.contains(&l) => {
                        stored.entry(l).or_default().push(*v)
                    }
                    _ => reads(*v),
                },
                Stmt::Return(Some(v)) => reads(*v),
                Stmt::If { .. }
                | Stmt::Loop { .. }
                | Stmt::Return(None)
                | Stmt::Break
                | Stmt::Continue
                | Stmt::Trap => {}
            }
        });
        // A same local a varying computation loads: what was stored to it is read.
        // (`same_values` makes a local varying if any store to it is.)
        let mut firsts: Vec<ValueId> = Vec::new();
        let mut done: HashSet<ValueId> = HashSet::new();
        for v in read {
            if done.insert(v) {
                firsts.push(v);
            }
        }
        // Each read value's cone: the same values it's computed from that cost work. Cones
        // that share such a value are reported once, at the read value with the costliest
        // cone, which is the work reported: values that only share a cheap one are no bigger
        // a computation together than apart.
        // The same values computed only from constants and loops' counters: through pure
        // operations, and locals whose every store is of such a value, at such indices, under
        // such conditions only, in loops left on such conditions only. They're the loop's own
        // structure (`(f32(i) + 1.0) / f32(steps)`): computed from nothing the command gives,
        // so nothing to compute once and pass in (§18.13). Not counted.
        let fixed = fixed_values(f, &def);
        let mut cost: std::collections::HashMap<ValueId, Option<u64>> =
            std::collections::HashMap::new();
        // How many times each value is computed an invocation: the passes of the loops it's
        // in, where the count bounds them (§18.11). A loop it can't bound counts once here.
        let passes = crate::work::loop_passes(m, id);
        let mut times: std::collections::HashMap<ValueId, u64> = std::collections::HashMap::new();
        fn times_in(
            b: &[Stmt],
            k: u64,
            passes: &std::collections::HashMap<usize, u32>,
            out: &mut std::collections::HashMap<ValueId, u64>,
        ) {
            for s in b {
                match s {
                    Stmt::Let(v, _) => {
                        out.insert(*v, k);
                    }
                    Stmt::If { then, else_, .. } => {
                        times_in(then, k, passes, out);
                        times_in(else_, k, passes, out);
                    }
                    Stmt::Loop { body, continuing } => {
                        let p = passes.get(&(body.as_ptr() as usize)).copied().unwrap_or(1);
                        let k = k.saturating_mul(u64::from(p.max(1)));
                        times_in(body, k, passes, out);
                        times_in(continuing, k, passes, out);
                    }
                    _ => {}
                }
            }
        }
        times_in(&f.body, 1, &passes, &mut times);
        let mut cones: Vec<(ValueId, Vec<ValueId>)> = Vec::new();
        for v in firsts {
            let mut stack = vec![v];
            let mut cone: HashSet<ValueId> = HashSet::new();
            let mut costed = Vec::new();
            while let Some(x) = stack.pop() {
                if !cone.insert(x) {
                    continue;
                }
                let Some(&(e, _)) = def.get(&x) else { continue };
                if !matches!(e, Expr::Load(_) | Expr::Const(_) | Expr::Zero(_)) {
                    let k = times.get(&x).copied().unwrap_or(1);
                    let w = *cost.entry(x).or_insert_with(|| {
                        if fixed.contains(&x) {
                            return Some(0);
                        }
                        self.work.of(f, e).map(|w| w.saturating_mul(k))
                    });
                    if w != Some(0) {
                        costed.push(x);
                    }
                }
                e.for_each_value(&mut |y| {
                    if same.contains(&y) {
                        stack.push(y);
                    }
                });
                if let Expr::Load(p) = e
                    && let PlaceRoot::Local(l) = p.root
                {
                    stack.extend(stored.get(&l).into_iter().flatten().copied());
                }
            }
            if !costed.is_empty() {
                cones.push((v, costed));
            }
        }
        // Union-find over the cones, by the costed values they share.
        let mut parent: Vec<usize> = (0..cones.len()).collect();
        fn find(p: &mut [usize], i: usize) -> usize {
            let mut r = i;
            while p[r] != r {
                r = p[r];
            }
            p[i] = r;
            r
        }
        let mut owner: std::collections::HashMap<ValueId, usize> = std::collections::HashMap::new();
        for (i, (_, costed)) in cones.iter().enumerate() {
            for x in costed {
                match owner.get(x) {
                    Some(&j) => {
                        let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                        parent[a] = b;
                    }
                    None => {
                        owner.insert(*x, i);
                    }
                }
            }
        }
        let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
        for i in 0..cones.len() {
            let r = find(&mut parent, i);
            groups.entry(r).or_default().push(i);
        }
        let sum = |xs: &[ValueId]| -> Option<u64> {
            xs.iter().try_fold(0u64, |a, x| cost[x].map(|w| a.saturating_add(w)))
        };
        for members in groups.values() {
            // The read value whose own cone costs the most.
            let Some(&top) = members.iter().max_by_key(|&&i| sum(&cones[i].1).unwrap_or(u64::MAX))
            else {
                continue;
            };
            let work = sum(&cones[top].1);
            if work.is_none_or(|w| w >= self.least) {
                let at = def[&cones[top].0].1;
                self.out.push(SameWork { function: f.name.clone(), at, work });
            }
        }
        for (g, bits) in calls {
            self.function(g, bits);
        }
    }
}

/// A store to a local, with the values it depends on beyond the one stored: its indices, the
/// conditions of the branches it's in, and those of the exits of the loops it's in.
struct LocalStore {
    local: LocalId,
    value: ValueId,
    on: Vec<ValueId>,
}

/// The values of `def` (a function's same values) computed only from constants and locals
/// that only ever hold such values, whichever invocation, pass or path: see `Sites::function`.
fn fixed_values(
    f: &Function,
    def: &std::collections::HashMap<ValueId, (&Expr, Option<wrela_diag::Span>)>,
) -> HashSet<ValueId> {
    // The conditions of the branches in `b` that leave a loop (or the function) under them.
    fn exits(b: &[Stmt], out: &mut Vec<ValueId>) {
        fn leaves(b: &[Stmt]) -> bool {
            b.iter().any(|s| match s {
                Stmt::Break | Stmt::Continue | Stmt::Return(_) => true,
                Stmt::If { then, else_, .. } => leaves(then) || leaves(else_),
                Stmt::Loop { body, continuing } => leaves(body) || leaves(continuing),
                _ => false,
            })
        }
        for s in b {
            match s {
                Stmt::If { cond, then, else_ } => {
                    if leaves(then) || leaves(else_) {
                        out.push(*cond);
                    }
                    exits(then, out);
                    exits(else_, out);
                }
                Stmt::Loop { body, continuing } => {
                    exits(body, out);
                    exits(continuing, out);
                }
                _ => {}
            }
        }
    }
    fn walk(b: &[Stmt], on: &mut Vec<ValueId>, out: &mut Vec<LocalStore>) {
        for s in b {
            match s {
                Stmt::If { cond, then, else_ } => {
                    on.push(*cond);
                    walk(then, on, out);
                    walk(else_, on, out);
                    on.pop();
                }
                Stmt::Loop { body, continuing } => {
                    let n = on.len();
                    exits(body, on);
                    exits(continuing, on);
                    walk(body, on, out);
                    walk(continuing, on, out);
                    on.truncate(n);
                }
                Stmt::Store(p, v) => {
                    if let PlaceRoot::Local(l) = p.root {
                        let mut deps = on.clone();
                        for q in &p.path {
                            if let crate::Proj::Index(i) = q {
                                deps.push(*i);
                            }
                        }
                        out.push(LocalStore { local: l, value: *v, on: deps });
                    }
                }
                _ => {}
            }
        }
    }
    let mut stores = Vec::new();
    walk(&f.body, &mut Vec::new(), &mut stores);
    // A local reached other than by a load and a store (lent, its address taken) is never
    // fixed.
    let mut reached: HashSet<LocalId> = HashSet::new();
    visit::walk(&f.body, &mut |s| {
        let (Stmt::Let(_, e) | Stmt::Eval(e)) = s else { return };
        let mut place = |p: &Place| {
            if let PlaceRoot::Local(l) = p.root {
                reached.insert(l);
            }
        };
        match e {
            Expr::Addr(p) | Expr::Run(p) | Expr::ArrayLength(p) | Expr::Atomic(_, p, _) => place(p),
            Expr::Call(_, args) => {
                for a in args {
                    if let Arg::Place(p) = a {
                        place(p);
                    }
                }
            }
            _ => {}
        }
    });
    let mut fixed: HashSet<ValueId> = def.keys().copied().collect();
    loop {
        let local_fixed = |l: LocalId, fixed: &HashSet<ValueId>| {
            !reached.contains(&l)
                && stores
                    .iter()
                    .filter(|st| st.local == l)
                    .all(|st| fixed.contains(&st.value) && st.on.iter().all(|c| fixed.contains(c)))
        };
        let keep: HashSet<ValueId> = fixed
            .iter()
            .copied()
            .filter(|x| {
                let (e, _) = def[x];
                let pure = match e {
                    Expr::Const(_) | Expr::Zero(_) => return true,
                    Expr::Load(p) => match p.root {
                        PlaceRoot::Local(l) => local_fixed(l, &fixed),
                        _ => false,
                    },
                    Expr::Unary(..)
                    | Expr::Binary(..)
                    | Expr::Builtin(..)
                    | Expr::Construct(..)
                    | Expr::Extract(..)
                    | Expr::ExtractDyn(..)
                    | Expr::Splat(..)
                    | Expr::Swizzle(..)
                    | Expr::Convert(..)
                    | Expr::Bitcast(..)
                    | Expr::Select { .. } => true,
                    _ => false,
                };
                let mut all = pure;
                e.for_each_value(&mut |y| all &= fixed.contains(&y));
                all
            })
            .collect();
        if keep.len() == fixed.len() {
            return fixed;
        }
        fixed = keep;
    }
}
