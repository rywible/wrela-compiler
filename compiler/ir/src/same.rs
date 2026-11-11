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
    analyse(m, f, args, &HashSet::new()).0
}

/// The same values, and the locals that vary. A call to a function in `unsteady` varies,
/// whatever it's given (before flattening, a function that reads what an invocation writes).
fn analyse(
    m: &Module,
    f: &Function,
    args: &[bool],
    unsteady: &HashSet<u32>,
) -> (HashSet<ValueId>, HashSet<LocalId>) {
    let mut a = Analysis {
        m,
        f,
        args,
        unsteady,
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
    unsteady: &'a HashSet<u32>,
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
            Expr::Call(g, args) => {
                !self.unsteady.contains(&g.0)
                    && args.iter().all(|a| matches!(a, Arg::Value(v) if self.same.contains(v)))
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
        let (same, varying) = analyse(m, f, &args, &HashSet::new());
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
        // such conditions only, in loops left on such conditions only (conditions since the
        // block the local lives in alone was entered: what led there doesn't change what it
        // holds). They're the loop's own structure (`(f32(i) + 1.0) / f32(steps)`): computed
        // from nothing the command gives, so nothing to compute once and pass in (§18.13).
        // Not counted.
        let fixed = fixed_values(f, &def);
        let mut cost: std::collections::HashMap<ValueId, Option<u64>> =
            std::collections::HashMap::new();
        // How many times each value is computed an invocation: the passes of the loops it's
        // in, where the count bounds them (§18.11). A loop it can't bound counts once here.
        let times = times_computed(m, id, f);
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

/// How many times each value of `f` (function `id` of `m`) is computed an invocation: the passes
/// of the loops it's in, where the count bounds them (§18.11). A loop it can't bound counts once.
fn times_computed(m: &Module, id: FuncId, f: &Function) -> std::collections::HashMap<ValueId, u64> {
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
    let passes = crate::work::loop_passes(m, id);
    let mut times = std::collections::HashMap::new();
    times_in(&f.body, 1, &passes, &mut times);
    times
}

/// A function GPU code calls, each invocation, with few numbers that vary between invocations
/// (§18.13): all else it's given is the same for every invocation. Such a call is a function of
/// those few numbers for the whole command (a direction's three, a distance's one): computed
/// once over their range into a table, each invocation could read it instead.
#[derive(Clone, Debug, PartialEq)]
pub struct FewInputs {
    /// The function called, as `name` names it.
    pub function: String,
    /// The most numbers that vary at any of its calls (a `vec3`'s three; a struct's all, if any
    /// of it varies).
    pub varying: u32,
    /// How many times an invocation calls it, as §18.11's count walks the code: each call's
    /// loops' passes (one, for a loop the count can't bound), but every side of a branch it
    /// doesn't know taken (not only the costlier).
    pub calls: u64,
    /// All those calls' work, each given what's known of its arguments (`None`: unbounded).
    pub work: Option<u64>,
    /// Its first call, where it is.
    pub at: Option<wrela_diag::Span>,
}

/// The most varying numbers a call may take for [`few_input_calls`].
pub const FEW: u32 = 3;

/// The least work a call must do, each, for [`few_input_calls`]: more than a table's read (a
/// sample, and its coordinates worked out).
pub const TABLE_READ: u64 = 2 * crate::work::SAMPLE;

/// The functions each entry point of `m` calls with at most [`FEW`] numbers that vary (and at
/// least one), each call more work than [`TABLE_READ`] and all of them at least `least` an
/// invocation, most work first: what a table over those numbers could replace. `name` names a
/// function (calls to functions it names alike are one). Such a call is reported whole, not
/// the calls in it; a call lent a place, given only the invocation's place, returning nothing,
/// or to a function that writes, synchronises, takes derivatives or reads what the command
/// writes, is looked into instead.
pub fn few_input_calls(
    m: &Module,
    least: u64,
    name: &dyn Fn(FuncId) -> String,
) -> Vec<Vec<FewInputs>> {
    let mut few = Few {
        m,
        runs: crate::work::CallRuns::new(m),
        pure: std::collections::HashMap::new(),
        shapes: std::collections::HashMap::new(),
        memo: std::collections::HashMap::new(),
    };
    let mut out = Vec::new();
    for e in &m.entry_points {
        let mut found: Vec<FewInputs> = Vec::new();
        if !matches!(e.stage, Stage::Compute { workgroup_size: [1, 1, 1] }) {
            let given = Given { same: Vec::new(), placed: Vec::new() };
            let known = vec![None; m.functions[e.function.0 as usize].params.len()];
            for h in few.calls(e.function, given, known).iter() {
                let function = name(h.g);
                let work = (!h.unbounded).then_some(h.work);
                match found.iter_mut().find(|c| c.function == function) {
                    Some(c) => {
                        c.varying = c.varying.max(h.varying);
                        c.calls = c.calls.saturating_add(h.calls);
                        c.work = c.work.zip(work).map(|(a, b)| a.saturating_add(b));
                    }
                    None => found.push(FewInputs {
                        function,
                        varying: h.varying,
                        calls: h.calls,
                        work,
                        at: h.at,
                    }),
                }
            }
        }
        found.retain(|c| c.work.is_none_or(|w| w >= least));
        found.sort_by(|a, b| {
            let w = |x: &FewInputs| x.work.unwrap_or(u64::MAX);
            w(b).cmp(&w(a)).then(a.function.cmp(&b.function))
        });
        out.push(found);
    }
    out
}

/// A call `few_input_calls` reports, by the function called: in one call of the function it's
/// found from.
#[derive(Clone)]
struct Hit {
    g: FuncId,
    varying: u32,
    calls: u64,
    work: u64,
    unbounded: bool,
    at: Option<wrela_diag::Span>,
}

/// A call in a function, as `few_input_calls` sees it, given what the function's given: the
/// function called, the numbers that vary in what it's given, whether it's reported (else
/// what its function's given), and where it is.
struct Site {
    g: FuncId,
    varying: u32,
    hit: bool,
    given: Given,
    at: Option<wrela_diag::Span>,
}

/// A function's calls, by each call's address.
type CallSites = std::rc::Rc<Vec<(usize, Site)>>;

struct Few<'a> {
    m: &'a Module,
    runs: crate::work::CallRuns<'a>,
    /// `pure_or_math`'s memo.
    pure: std::collections::HashMap<u32, bool>,
    /// Each function's calls, by the call's address, given what it's given.
    shapes: std::collections::HashMap<(u32, Given), CallSites>,
    /// The calls reported in one call of a function, given what it's given and what's known of
    /// its arguments.
    memo: std::collections::HashMap<(u32, Given, crate::work::Known), std::rc::Rc<Vec<Hit>>>,
}

impl Few<'_> {
    /// The calls reported in one call of function `id`, given which of its parameters are same
    /// and placed, and what's known of its arguments: each function's calls summed.
    fn calls(
        &mut self,
        id: FuncId,
        given: Given,
        known: crate::work::Known,
    ) -> std::rc::Rc<Vec<Hit>> {
        let key = (id.0, given.clone(), known.clone());
        if let Some(h) = self.memo.get(&key) {
            return h.clone();
        }
        let sites = self.shape(id, &given);
        let runs = self.runs.of(id, &known);
        let mut out: Vec<Hit> = Vec::new();
        let add = |out: &mut Vec<Hit>, h: Hit| match out.iter_mut().find(|x| x.g == h.g) {
            Some(x) => {
                x.varying = x.varying.max(h.varying);
                x.calls = x.calls.saturating_add(h.calls);
                x.work = x.work.saturating_add(h.work);
                x.unbounded |= h.unbounded;
            }
            None => out.push(h),
        };
        let (runs, sure) = runs;
        for (addr, site) in sites.iter() {
            // (A call that doesn't run, on a side of a branch on a constant not taken; or, the
            // count not sure, one it didn't reach: once, at least.)
            let unknown;
            let r = match runs.get(addr) {
                Some(r) => r,
                None if sure => continue,
                None => {
                    let n = self.m.functions[site.g.0 as usize].params.len();
                    unknown = crate::work::SiteRuns {
                        times: 1,
                        work: 0,
                        unbounded: true,
                        known: vec![(vec![None; n], 1)],
                    };
                    &unknown
                }
            };
            // (Unbounded, a call's own work given nothing of its arguments says if it's cheap.)
            let each = if r.unbounded {
                let n = self.m.functions[site.g.0 as usize].params.len();
                self.runs.work_of(site.g, &vec![None; n])
            } else {
                Some(r.work / r.times.max(1))
            };
            if site.hit && each.is_none_or(|w| w > TABLE_READ) {
                let h = Hit {
                    g: site.g,
                    varying: site.varying,
                    calls: r.times,
                    work: r.work,
                    unbounded: r.unbounded,
                    at: site.at,
                };
                add(&mut out, h);
                continue;
            }
            for (k, n) in r.known.iter().filter(|(_, n)| *n > 0) {
                for h in self.calls(site.g, site.given.clone(), k.clone()).iter() {
                    let h = Hit {
                        calls: h.calls.saturating_mul(*n),
                        work: h.work.saturating_mul(*n),
                        ..h.clone()
                    };
                    add(&mut out, h);
                }
            }
        }
        let out = std::rc::Rc::new(out);
        self.memo.insert(key, out.clone());
        out
    }

    /// Function `id`'s calls, given which of its parameters are same and placed.
    fn shape(&mut self, id: FuncId, given: &Given) -> CallSites {
        let key = (id.0, given.clone());
        if let Some(s) = self.shapes.get(&key) {
            return s.clone();
        }
        let m = self.m;
        let f = &m.functions[id.0 as usize];
        // (Before flattening, a call may read what an invocation writes: its result varies.)
        let mut unsteady = HashSet::new();
        visit::walk(&f.body, &mut |s| {
            if let Stmt::Let(_, Expr::Call(g, _)) | Stmt::Eval(Expr::Call(g, _)) = s {
                unsteady.insert(g.0);
            }
        });
        let pure = &mut self.pure;
        unsteady.retain(|g| !steady_fn(m, FuncId(*g), pure));
        let (same, _) = analyse(m, f, &given.same, &unsteady);
        let placed = placed_values(m, f, &given.placed, &same, pure);
        let mut sites = Vec::new();
        let mut here = None;
        let mut found = Vec::new();
        visit::walk(&f.body, &mut |s| {
            let (v, e) = match s {
                Stmt::At(span) => {
                    here = Some(*span);
                    return;
                }
                Stmt::Let(v, e @ Expr::Call(..)) => (Some(*v), e),
                Stmt::Eval(e @ Expr::Call(..)) => (None, e),
                _ => return,
            };
            found.push((v, e, here));
        });
        for (v, e, at) in found {
            let Expr::Call(g, a) = e else { continue };
            let bits: Vec<bool> =
                a.iter().map(|a| matches!(a, Arg::Value(x) if same.contains(x))).collect();
            let at_place: Vec<bool> =
                a.iter().map(|a| matches!(a, Arg::Value(x) if placed.contains(x))).collect();
            // (Given only the invocation's place and same values: a function of the place.)
            let of_place = a.iter().all(|a| match a {
                Arg::Value(x) => same.contains(x) || placed.contains(x),
                Arg::Place(_) => false,
            });
            let lent = a.iter().any(|a| matches!(a, Arg::Place(_)));
            let varying: u32 = a
                .iter()
                .map(|a| match a {
                    Arg::Value(x) if !same.contains(x) => scalars(m, f.value_ty(*x)),
                    _ => 0,
                })
                .sum();
            // (A call that returns nothing is for its effect: no table holds that.)
            let hit = v.is_some()
                && !lent
                && !of_place
                && (1..=FEW).contains(&varying)
                && pure_fn(m, *g, pure);
            let given = Given { same: bits, placed: at_place };
            sites.push((e as *const Expr as usize, Site { g: *g, varying, hit, given, at }));
        }
        let sites = std::rc::Rc::new(sites);
        self.shapes.insert(key, sites.clone());
        sites
    }
}

/// What a function is given, for `few_input_calls`: which of its parameters are same, and which
/// are the invocation's place (`placed`).
#[derive(Clone, PartialEq, Eq, Hash)]
struct Given {
    same: Vec<bool>,
    placed: Vec<bool>,
}

/// The values of `f` computed by arithmetic from the invocation's place (its builtin inputs,
/// and the parameters `placed` says), and same values: a function of the place alone, where a
/// table over them is as large as the dispatch (a fragment shader's pixel, a direction through
/// it). Calls given only those are looked into rather than reported.
fn placed_values(
    m: &Module,
    f: &Function,
    placed: &[bool],
    same: &HashSet<ValueId>,
    pure: &mut std::collections::HashMap<u32, bool>,
) -> HashSet<ValueId> {
    let (stores, reached) = local_stores(f);
    // Optimistic about locals: one each of whose stores is of a placed or same value, at placed or
    // same indices, under placed or same conditions, is placed; each round finds more that
    // aren't, until a round finds none.
    let mut locals: HashSet<LocalId> =
        stores.iter().map(|st| st.local).filter(|l| !reached.contains(l)).collect();
    loop {
        let out = placed_round(m, f, placed, same, pure, &locals);
        let ok = |v: &ValueId| out.contains(v) || same.contains(v);
        let keep: HashSet<LocalId> = locals
            .iter()
            .copied()
            .filter(|l| {
                stores
                    .iter()
                    .filter(|st| st.local == *l)
                    .all(|st| ok(&st.value) && st.on.iter().all(ok))
            })
            .collect();
        if keep.len() == locals.len() {
            return out;
        }
        locals = keep;
    }
}

/// One round of `placed_values`, given the locals taken as placed.
fn placed_round(
    m: &Module,
    f: &Function,
    placed: &[bool],
    same: &HashSet<ValueId>,
    pure: &mut std::collections::HashMap<u32, bool>,
    locals: &HashSet<LocalId>,
) -> HashSet<ValueId> {
    let mut out: HashSet<ValueId> = HashSet::new();
    visit::walk(&f.body, &mut |s| {
        let Stmt::Let(v, e) = s else { return };
        if let Expr::Load(p) = e
            && let PlaceRoot::Local(l) = p.root
            && locals.contains(&l)
            && !same.contains(v)
            && p.path.iter().all(|x| match x {
                Proj::Index(i) => out.contains(i) || same.contains(i),
                Proj::Field(_) | Proj::Comp(_) => true,
            })
        {
            out.insert(*v);
            return;
        }
        let mut some = false;
        let mut all = true;
        e.for_each_value(&mut |x| {
            if out.contains(&x) {
                some = true;
            } else if !same.contains(&x) {
                all = false;
            }
        });
        let ok = match e {
            Expr::EntryInput(_) => true,
            Expr::Param(i) => placed.get(*i as usize).copied().unwrap_or(false),
            Expr::Unary(..)
            | Expr::Binary(..)
            | Expr::Construct(..)
            | Expr::Extract(..)
            | Expr::Splat(..)
            | Expr::Swizzle(..)
            | Expr::Convert(..)
            | Expr::Bitcast(..)
            | Expr::Select { .. }
            | Expr::Builtin(..) => some && all,
            // (A pure function of the place is too: a hash of the pixel, a jitter.)
            Expr::Call(g, a) => {
                some && all && a.iter().all(|a| matches!(a, Arg::Value(_))) && math_fn(m, *g, pure)
            }
            _ => false,
        };
        if ok {
            out.insert(*v);
        }
    });
    out
}

/// How many numbers a value of type `t` holds.
fn scalars(m: &Module, t: TypeId) -> u32 {
    match m.types.get(t) {
        TypeDef::Scalar(_) | TypeDef::Atomic(_) => 1,
        TypeDef::Vector(_, n) => u32::from(*n),
        TypeDef::Matrix(c, r) => u32::from(*c) * u32::from(*r),
        TypeDef::Struct { fields, .. } => fields.iter().map(|(_, f)| scalars(m, *f)).sum(),
        TypeDef::Enum { variants, .. } => {
            1 + variants.iter().filter_map(|(_, p)| p.map(|p| scalars(m, p))).sum::<u32>()
        }
        TypeDef::Array(e, n) => scalars(m, *e).saturating_mul(*n),
        TypeDef::RuntimeArray(_) | TypeDef::Run(_) | TypeDef::Ptr(_) => u32::MAX / 4,
    }
}

/// Whether function `g`, and all it calls, computes a value from its parameters and what a
/// command gives every invocation alike, and nothing else: no writes but to its own locals, no
/// reads of what the command writes, no derivatives, no subgroups, no barriers, no discards.
fn pure_fn(m: &Module, g: FuncId, memo: &mut std::collections::HashMap<u32, bool>) -> bool {
    pure_or(m, g, memo, Pureness::Pure)
}

/// Whether function `g`, and all it calls, is [`pure_fn`] and reads nothing but its parameters
/// and uniform blocks (`math`): no texture, no buffer. Given only the invocation's place, its
/// result is a function of the place.
fn math_fn(m: &Module, g: FuncId, memo: &mut std::collections::HashMap<u32, bool>) -> bool {
    pure_or(m, g, memo, Pureness::Math)
}

/// Whether function `g`, and all it calls, reads nothing that varies but its parameters: given
/// same values, its result is the same for every invocation (it may sample, take derivatives
/// and discard, as [`pure_fn`] may not).
fn steady_fn(m: &Module, g: FuncId, memo: &mut std::collections::HashMap<u32, bool>) -> bool {
    pure_or(m, g, memo, Pureness::Steady)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pureness {
    Steady,
    Pure,
    Math,
}

fn pure_or(
    m: &Module,
    g: FuncId,
    memo: &mut std::collections::HashMap<u32, bool>,
    how: Pureness,
) -> bool {
    // (The memo holds all three: each under the function's index with its own top bits.)
    let key = g.0
        | match how {
            Pureness::Pure => 0,
            Pureness::Math => 1 << 31,
            Pureness::Steady => 1 << 30,
        };
    if let Some(&p) = memo.get(&key) {
        return p;
    }
    // (Recursion isn't GPU code's: a cycle reads as pure while it's looked at.)
    memo.insert(key, true);
    let f = &m.functions[g.0 as usize];
    let math = how == Pureness::Math;
    let steady = how == Pureness::Steady;
    let mut ok = true;
    let mut callees = Vec::new();
    let readable = |p: &Place| match p.root {
        PlaceRoot::Resource(r) => match m.resources[r.0 as usize].kind {
            ResourceKind::Uniform { .. } => true,
            ResourceKind::StorageRead | ResourceKind::Texture { .. } => !math,
            _ => false,
        },
        PlaceRoot::Local(_) | PlaceRoot::Param(_) => true,
        PlaceRoot::Ptr(_) | PlaceRoot::Data(_) => false,
    };
    visit::walk(&f.body, &mut |s| match s {
        Stmt::Store(p, _) => ok &= matches!(p.root, PlaceRoot::Local(_)),
        Stmt::Let(_, e) | Stmt::Eval(e) => match e {
            Expr::Load(p) => ok &= readable(p),
            Expr::Call(h, a) => {
                ok &= a.iter().all(|a| matches!(a, Arg::Value(_)));
                callees.push(*h);
            }
            Expr::Builtin(b, _) => {
                ok &= steady || !matches!(b, Builtin::Dpdx | Builtin::Dpdy | Builtin::Fwidth)
            }
            Expr::Texture(op, ..) => {
                ok &= steady
                    || !math
                        && !matches!(
                            op,
                            TextureOp::Sample | TextureOp::SampleBias | TextureOp::SampleCompare
                        )
            }
            Expr::Discard => ok &= steady,
            Expr::EntryInput(_)
            | Expr::Run(_)
            | Expr::Addr(_)
            | Expr::Host(..)
            | Expr::Mem(..)
            | Expr::TextureStore(..)
            | Expr::Atomic(..)
            | Expr::Subgroup(..)
            | Expr::Barrier => ok = false,
            _ => {}
        },
        _ => {}
    });
    for h in callees {
        ok = ok && pure_or(m, h, memo, how);
    }
    memo.insert(key, ok);
    ok
}

/// Each store to a local of `f`, with what it depends on beyond the value stored, and the locals
/// reached other than by loads and stores (lent, their address taken).
fn local_stores(f: &Function) -> (Vec<LocalStore>, HashSet<LocalId>) {
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
    // The locals that live in `b` alone: every mention of one (`mentions`, the function's) is
    // in `b` or nested in it, and the first statement of `b` to mention it sets the whole of
    // it. Whenever `b` is entered such a local starts afresh, so what it holds there doesn't
    // depend on what led to `b`.
    fn own(b: &[Stmt], mentions: &[u32]) -> Vec<LocalId> {
        // By local: whether its first mention sets the whole of it, and its mentions.
        let mut here: std::collections::HashMap<LocalId, (bool, u32)> = Default::default();
        for s in b {
            let whole = match s {
                Stmt::Store(p, _) if p.path.is_empty() => p.root_local(),
                _ => None,
            };
            visit::walk(std::slice::from_ref(s), &mut |t| {
                t.for_each_place(&mut |p| {
                    if let Some(l) = p.root_local() {
                        here.entry(l).or_insert((whole == Some(l), 0)).1 += 1;
                    }
                })
            });
        }
        here.into_iter()
            .filter(|&(l, (set, n))| set && n == mentions[l.index()])
            .map(|(l, _)| l)
            .collect()
    }
    struct Walk<'a> {
        mentions: &'a [u32],
        // By local: how many of the conditions leading to the innermost block it lives in
        // alone (`own`) there are. A store to it doesn't depend on those.
        led: Vec<usize>,
        out: Vec<LocalStore>,
    }
    fn walk(b: &[Stmt], on: &mut Vec<ValueId>, w: &mut Walk) {
        for l in own(b, w.mentions) {
            w.led[l.index()] = on.len();
        }
        for s in b {
            match s {
                Stmt::If { cond, then, else_ } => {
                    on.push(*cond);
                    walk(then, on, w);
                    walk(else_, on, w);
                    on.pop();
                }
                Stmt::Loop { body, continuing } => {
                    let n = on.len();
                    exits(body, on);
                    exits(continuing, on);
                    walk(body, on, w);
                    walk(continuing, on, w);
                    on.truncate(n);
                }
                Stmt::Store(p, v) => {
                    if let PlaceRoot::Local(l) = p.root {
                        let mut deps = on[w.led[l.index()]..].to_vec();
                        for q in &p.path {
                            if let crate::Proj::Index(i) = q {
                                deps.push(*i);
                            }
                        }
                        w.out.push(LocalStore { local: l, value: *v, on: deps });
                    }
                }
                _ => {}
            }
        }
    }
    let mut mentions = vec![0; f.locals.len()];
    visit::count_local_mentions(&f.body, &mut mentions);
    let mut w = Walk { mentions: &mentions, led: vec![0; f.locals.len()], out: Vec::new() };
    walk(&f.body, &mut Vec::new(), &mut w);
    let stores = w.out;
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
    (stores, reached)
}

/// A store to a local, with the values it depends on beyond the one stored: its indices, the
/// conditions of the branches it's in, and those of the exits of the loops it's in. Not the
/// conditions that lead to a block the local lives in alone (`local_stores`' `own`): a loop
/// under `if r.learned != 0 { .. }` counts as it would outside.
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
    let (stores, reached) = local_stores(f);
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
