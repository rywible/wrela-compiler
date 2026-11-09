//! Work (GPU, language.md §18.11): the most work one call of a function can do, counted from its
//! code before the shader compiler's own optimizations, in units of about one 32-bit operation
//! on one invocation. A count is the same on every machine, so a build can check it
//! (`@budget(work: n)`); it is an upper bound to compare code by, not a time.
//!
//! The count walks the code as it would run, knowing only what the code makes constant: its
//! constants, `const` parameters folded into a pipeline, and the values computed from them. So
//! a branch on a constant counts the side it takes; a branch on anything else counts the costlier
//! side; and a loop counts each pass it makes until an exit it's sure of (a `for` over a constant
//! range, a `while` whose counter reaches a constant). A loop with no such exit within
//! [`MOST_PASSES`] passes is unbounded, and so is the work of what holds it.
//!
//! What each operation counts is [`cost`]'s table: arithmetic one a component, a division or a
//! transcendental four, a read of a storage buffer eight, a texture sample sixteen.

use crate::*;
use std::collections::HashMap;
use wrela_diag::Span;

/// The most passes a loop is followed for: past it, the loop's work is unbounded.
pub const MOST_PASSES: u32 = 1 << 12;

/// The most statements the count follows for one entry point (a guard on the count's own time):
/// past it, the work is unbounded.
pub const MOST_STEPS: u64 = 1 << 24;

/// Work units: arithmetic on one component.
pub const OP: u64 = 1;
/// A division or remainder, a square root or a transcendental, on one component.
pub const SLOW_OP: u64 = 4;
/// A read or a write of a storage buffer.
pub const MEMORY: u64 = 8;
/// A read or a write of workgroup memory.
pub const SHARED: u64 = 2;
/// A texture sample or gather.
pub const SAMPLE: u64 = 16;
/// A texel load or a storage texture's write.
pub const TEXEL: u64 = 8;
/// An atomic operation.
pub const ATOMIC: u64 = 16;

/// A function over its budget: where its `@budget` is, the budget, its work (`None`:
/// unbounded, at `unbounded`'s loop), and the calls that do the most of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Breach {
    pub function: String,
    pub at: Span,
    pub budget: u64,
    pub work: Option<u64>,
    pub unbounded: Option<Span>,
    /// The functions it calls itself, by name, with their work, most first (at most three).
    pub biggest: Vec<(String, u64)>,
}

/// An entry point's work: `None` when unbounded, with the loop that's unbounded if one is.
#[derive(Clone, Debug, PartialEq)]
pub struct EntryWork {
    pub work: Option<u64>,
    pub unbounded: Option<Span>,
    /// Each function over its `@budget` in this module, once each (the largest call of it).
    pub breaches: Vec<Breach>,
}

/// The work of each entry point of module `m`, in its order, and the functions over their
/// budgets (once each). One count serves the module, so a function two entry points call is
/// followed once for each set of known arguments.
pub fn module_work(m: &Module) -> (Vec<EntryWork>, Vec<Breach>) {
    let mut c = Counter {
        m,
        steps: 0,
        gave_up: false,
        memo: HashMap::new(),
        breaches: Vec::new(),
        passes: HashMap::new(),
    };
    let mut out = Vec::new();
    for e in &m.entry_points {
        (c.steps, c.gave_up) = (0, false);
        let r = c.call(e.function, &[]);
        let work = if r.unbounded || c.gave_up { None } else { Some(r.work) };
        out.push(EntryWork { work, unbounded: r.loop_at, breaches: Vec::new() });
    }
    (out, c.breaches)
}

/// The work of entry point `f` of module `m`, and the functions over their budgets.
pub fn entry_work(m: &Module, f: FuncId) -> EntryWork {
    let mut c = Counter {
        m,
        steps: 0,
        gave_up: false,
        memo: HashMap::new(),
        breaches: Vec::new(),
        passes: HashMap::new(),
    };
    let r = c.call(f, &[]);
    let work = if r.unbounded || c.gave_up { None } else { Some(r.work) };
    EntryWork { work, unbounded: r.loop_at, breaches: c.breaches }
}

/// The work of single expressions of a module's functions, knowing nothing of their values (a
/// call's is its function's, given nothing of its arguments). One count serves each call, so
/// a function called many times is followed once.
pub struct ExprWork<'a> {
    c: Counter<'a>,
}

impl<'a> ExprWork<'a> {
    pub fn new(m: &'a Module) -> ExprWork<'a> {
        ExprWork {
            c: Counter {
                m,
                steps: 0,
                gave_up: false,
                memo: HashMap::new(),
                breaches: Vec::new(),
                passes: HashMap::new(),
            },
        }
    }

    /// The most work `e`, in `f`, can do: `None` when unbounded.
    pub fn of(&mut self, f: &Function, e: &Expr) -> Option<u64> {
        let mut fr = Frame {
            f,
            args: Vec::new(),
            vals: HashMap::new(),
            work: 0,
            unbounded: false,
            loop_at: None,
            here: None,
            ret: None,
            calls: HashMap::new(),
            sites: None,
        };
        (self.c.steps, self.c.gave_up) = (0, false);
        self.c.expr(&mut fr, &mut State::default(), e);
        (!fr.unbounded && !self.c.gave_up).then_some(fr.work)
    }
}

/// The most passes each loop of function `f` makes, by its body's address (`as_ptr`), knowing
/// nothing of `f`'s parameters: a loop missing ends after a number of passes the count can't
/// bound.
pub fn loop_passes(m: &Module, f: FuncId) -> HashMap<usize, u32> {
    let mut c = Counter {
        m,
        steps: 0,
        gave_up: false,
        memo: HashMap::new(),
        breaches: Vec::new(),
        passes: HashMap::new(),
    };
    let args = vec![None; m.functions[f.0 as usize].params.len()];
    c.call(f, &args);
    if c.gave_up { HashMap::new() } else { c.passes }
}

#[derive(Clone, Debug)]
struct Called {
    work: u64,
    unbounded: bool,
    loop_at: Option<Span>,
    ret: Option<Const>,
}

/// A call, by its function and what's known of its arguments.
type CallKey = (u32, Known);

/// What's known of a call's arguments: of each, a kind and a value. Only what control can
/// depend on is told apart: integers and `bool`s.
pub type Known = Vec<Option<(u8, u32)>>;

fn known_of(args: &[Option<Const>]) -> Known {
    args.iter()
        .map(|a| match a {
            Some(Const::Bool(b)) => Some((0, u32::from(*b))),
            Some(Const::I32(i)) => Some((1, *i as u32)),
            Some(Const::U32(u)) => Some((2, *u)),
            _ => None,
        })
        .collect()
}

fn consts_of(k: &Known) -> Vec<Option<Const>> {
    k.iter()
        .map(|a| match a {
            Some((0, b)) => Some(Const::Bool(*b != 0)),
            Some((1, i)) => Some(Const::I32(*i as i32)),
            Some((2, u)) => Some(Const::U32(*u)),
            _ => None,
        })
        .collect()
}

/// A call's runs in one call of the function it's in ([`CallRuns`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SiteRuns {
    /// How many times it runs: each of its loops' passes (one, for a loop the count can't
    /// bound), every side of a branch the count doesn't know taken.
    pub times: u64,
    /// All those runs' work, each given what's known of its arguments then.
    pub work: u64,
    /// A run's work is unbounded, or the count gave up before it was sure of them all.
    pub unbounded: bool,
    /// What's known of its arguments, each way it differs between runs, with how many runs (a
    /// way may be left with none).
    pub known: Vec<(Known, u64)>,
}

/// What a [`CallRuns`] count has found so far: each call's runs, and each run's record (the
/// call, which of its `known`, how many runs, their work), so a loop the count can't bound can
/// take back all but a pass of what it added.
#[derive(Default)]
struct SiteLog {
    runs: HashMap<usize, SiteRuns>,
    log: Vec<(usize, u32, u64, u64)>,
    /// The loops found unbounded (by their bodies' addresses): met again, each runs one pass.
    endless: std::collections::HashSet<usize>,
}

impl SiteLog {
    fn record(&mut self, at: usize, known: Known, work: u64, unbounded: bool) {
        let r = self.runs.entry(at).or_default();
        r.times = r.times.saturating_add(1);
        r.work = r.work.saturating_add(work);
        r.unbounded |= unbounded;
        let k = match r.known.iter().position(|(j, _)| *j == known) {
            Some(k) => k,
            None => {
                r.known.push((known, 0));
                r.known.len() - 1
            }
        };
        r.known[k].1 += 1;
        self.log.push((at, k as u32, 1, work));
    }

    /// Scales the runs recorded since `from` down to one of the loop's `passes`: the count can't
    /// bound it. What was known of a call's arguments then is what all of those runs agree on.
    fn one_pass(&mut self, from: usize, passes: u64) {
        let tail = self.log.split_off(from);
        // (Each call: its runs, their work, and how many of them each way of `known` had.)
        type Added = (usize, u64, u64, Vec<(u32, u64)>);
        let mut each: Vec<Added> = Vec::new();
        for (at, k, n, w) in tail {
            let e = match each.iter().position(|e| e.0 == at) {
                Some(i) => &mut each[i],
                None => {
                    each.push((at, 0, 0, Vec::new()));
                    each.last_mut().expect("pushed")
                }
            };
            e.1 += n;
            e.2 = e.2.saturating_add(w);
            match e.3.iter_mut().find(|(j, _)| *j == k) {
                Some((_, m)) => *m += n,
                None => e.3.push((k, n)),
            }
        }
        let passes = passes.max(1);
        for (at, n, w, ks) in each {
            let r = self.runs.get_mut(&at).expect("a recorded call");
            let (n1, w1) = (n.div_ceil(passes), w.div_ceil(passes));
            r.times = r.times - n + n1;
            r.work = r.work - w + w1;
            let mut agreed: Option<Known> = None;
            for (k, m) in ks {
                let (known, count) = &mut r.known[k as usize];
                *count -= m;
                agreed = Some(match agreed {
                    None => known.clone(),
                    Some(a) => a
                        .iter()
                        .zip(known.iter())
                        .map(|(x, y)| if x == y { *x } else { None })
                        .collect(),
                });
            }
            let agreed = agreed.expect("a run");
            let k = match r.known.iter().position(|(j, _)| *j == agreed) {
                Some(k) => k,
                None => {
                    r.known.push((agreed, 0));
                    r.known.len() - 1
                }
            };
            r.known[k].1 += n1;
            self.log.push((at, k as u32, n1, w1));
        }
    }
}

/// The most statements a [`CallRuns`] follows in all (a guard on the count's own time): past
/// it, the calls of what's counted after are unbounded.
pub const MOST_CALL_RUN_STEPS: u64 = 1 << 26;

/// How each call in a function runs in one call of it, knowing what the count knows: its
/// constants, what's known of its arguments, and the values computed from them. One count
/// serves the calls it makes, so a function called many times is followed once for each set of
/// known arguments.
pub struct CallRuns<'a> {
    c: Counter<'a>,
    /// The statements followed so far, over every `of`.
    steps: u64,
}

impl<'a> CallRuns<'a> {
    pub fn new(m: &'a Module) -> CallRuns<'a> {
        CallRuns {
            c: Counter {
                m,
                steps: 0,
                gave_up: false,
                memo: HashMap::new(),
                breaches: Vec::new(),
                passes: HashMap::new(),
            },
            steps: 0,
        }
    }

    /// The most work one call of `f` can do given `known` of its arguments (`None`: unbounded).
    pub fn work_of(&mut self, f: FuncId, known: &Known) -> Option<u64> {
        (self.c.steps, self.c.gave_up) = (0, false);
        let c = self.c.call(f, &consts_of(known));
        (!c.unbounded && !self.c.gave_up).then_some(c.work)
    }

    /// Each call in function `f`'s own body, by the call expression's address, with its runs
    /// in one call of `f` given `known` of its arguments, and whether the count was sure of them
    /// all. Sure, a call missing doesn't run; not (past [`MOST_STEPS`] in `f`, or
    /// [`MOST_CALL_RUN_STEPS`] in all), each found is unbounded and one missing may run.
    pub fn of(&mut self, f: FuncId, known: &Known) -> (HashMap<usize, SiteRuns>, bool) {
        if self.steps > MOST_CALL_RUN_STEPS {
            return (HashMap::new(), false);
        }
        let func = &self.c.m.functions[f.0 as usize];
        let mut args = consts_of(known);
        args.resize(func.params.len(), None);
        let mut fr = Frame {
            f: func,
            args,
            vals: HashMap::new(),
            work: 0,
            unbounded: false,
            loop_at: None,
            here: None,
            ret: None,
            calls: HashMap::new(),
            sites: Some(SiteLog::default()),
        };
        (self.c.steps, self.c.gave_up) = (0, false);
        self.c.block(&mut fr, &mut State::default(), &func.body);
        self.steps = self.steps.saturating_add(self.c.steps);
        let mut runs = fr.sites.map(|s| s.runs).unwrap_or_default();
        let sure = !self.c.gave_up;
        if !sure {
            for r in runs.values_mut() {
                r.unbounded = true;
            }
        }
        (runs, sure)
    }
}

struct Counter<'a> {
    m: &'a Module,
    steps: u64,
    /// The count went past [`MOST_STEPS`]: what it found after is not the code's, so no
    /// function is reported over its budget from then on.
    gave_up: bool,
    memo: HashMap<CallKey, Called>,
    breaches: Vec<Breach>,
    /// The most passes each loop that ended made, by its body's address.
    passes: HashMap<usize, u32>,
}

/// How a block ended, when it's sure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Normal,
    Break,
    Continue,
    Return,
}

#[derive(Clone, Debug, Default)]
struct State {
    /// Locals whose whole value is known.
    locals: HashMap<LocalId, Const>,
}

impl State {
    fn join(&mut self, other: &State) {
        self.locals.retain(|l, c| other.locals.get(l) == Some(c));
    }
}

struct Frame<'f> {
    f: &'f Function,
    args: Vec<Option<Const>>,
    vals: HashMap<ValueId, Const>,
    work: u64,
    unbounded: bool,
    loop_at: Option<Span>,
    here: Option<Span>,
    /// What each sure return gave: `Some(None)` once two differ or one isn't known.
    ret: Option<Option<Const>>,
    /// Work of each function it calls itself.
    calls: HashMap<u32, u64>,
    /// Each call in its own body, by the call's address, and its runs ([`CallRuns`] only).
    sites: Option<SiteLog>,
}

impl<'a> Counter<'a> {
    fn call(&mut self, id: FuncId, args: &[Option<Const>]) -> Called {
        let key = (id.0, known_of(args));
        let args = consts_of(&key.1);
        if let Some(c) = self.memo.get(&key) {
            return c.clone();
        }
        let f = &self.m.functions[id.0 as usize];
        let mut fr = Frame {
            f,
            args,
            vals: HashMap::new(),
            work: 0,
            unbounded: false,
            loop_at: None,
            here: None,
            ret: None,
            calls: HashMap::new(),
            sites: None,
        };
        let mut st = State::default();
        self.block(&mut fr, &mut st, &f.body);
        let called = Called {
            work: fr.work,
            unbounded: fr.unbounded,
            loop_at: fr.loop_at,
            ret: fr.ret.flatten(),
        };
        if let Some(Budget { work: Some(budget), at, .. }) = f.budget
            && !self.gave_up
        {
            let over = called.unbounded || called.work > budget;
            if over {
                let mut biggest: Vec<(String, u64)> = fr
                    .calls
                    .into_iter()
                    .map(|(c, w)| (self.m.functions[c as usize].name.clone(), w))
                    .collect();
                biggest.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                biggest.truncate(3);
                let b = Breach {
                    function: f.name.clone(),
                    at,
                    budget,
                    work: (!called.unbounded).then_some(called.work),
                    unbounded: called.loop_at,
                    biggest,
                };
                match self.breaches.iter_mut().find(|x| x.at == at) {
                    Some(x) => {
                        if b.work.is_none() || x.work.is_some_and(|w| b.work.is_some_and(|v| v > w))
                        {
                            *x = b;
                        }
                    }
                    None => self.breaches.push(b),
                }
            }
        }
        if !self.gave_up {
            self.memo.insert(key, called.clone());
        }
        called
    }

    fn block(&mut self, fr: &mut Frame, st: &mut State, b: &[Stmt]) -> Flow {
        for s in b {
            self.steps += 1;
            if self.steps > MOST_STEPS {
                self.gave_up = true;
                fr.unbounded = true;
                return Flow::Return;
            }
            match self.stmt(fr, st, s) {
                Flow::Normal => {}
                other => return other,
            }
        }
        Flow::Normal
    }

    fn stmt(&mut self, fr: &mut Frame, st: &mut State, s: &Stmt) -> Flow {
        match s {
            Stmt::At(span) => fr.here = Some(*span),
            Stmt::Let(v, e) => {
                // A value a loop's pass defines again forgets what an earlier pass gave it.
                match self.expr(fr, st, e) {
                    Some(c) => fr.vals.insert(*v, c),
                    None => fr.vals.remove(v),
                };
            }
            Stmt::Eval(e) => {
                self.expr(fr, st, e);
            }
            Stmt::Store(p, v) => {
                fr.work = fr.work.saturating_add(self.place_cost(p, true));
                match (p.root.clone(), p.path.is_empty()) {
                    (PlaceRoot::Local(l), true) => match fr.vals.get(v) {
                        Some(c) => {
                            st.locals.insert(l, c.clone());
                        }
                        None => {
                            st.locals.remove(&l);
                        }
                    },
                    (PlaceRoot::Local(l), false) => {
                        st.locals.remove(&l);
                    }
                    _ => {}
                }
            }
            Stmt::If { cond, then, else_ } => {
                fr.work = fr.work.saturating_add(OP);
                match fr.vals.get(cond) {
                    Some(Const::Bool(true)) => return self.block(fr, st, then),
                    Some(Const::Bool(false)) => return self.block(fr, st, else_),
                    _ => return self.both(fr, st, then, else_),
                }
            }
            Stmt::Loop { body, continuing } => return self.lp(fr, st, body, continuing),
            Stmt::Break => return Flow::Break,
            Stmt::Continue => return Flow::Continue,
            Stmt::Return(v) => {
                let c = v.and_then(|v| fr.vals.get(&v).cloned());
                fr.ret = Some(match fr.ret.take() {
                    None => c,
                    Some(prev) => prev.filter(|p| Some(p) == c.as_ref()),
                });
                return Flow::Return;
            }
            Stmt::Trap => return Flow::Return,
        }
        Flow::Normal
    }

    /// An `if` on a condition the count doesn't know: the costlier side's work, and what's
    /// known after it on the sides that go on.
    fn both(&mut self, fr: &mut Frame, st: &mut State, then: &[Stmt], else_: &[Stmt]) -> Flow {
        let start = fr.work;
        let mut a = st.clone();
        let fa = self.block(fr, &mut a, then);
        let wa = fr.work - start;
        fr.work = start;
        let mut b = st.clone();
        let fb = self.block(fr, &mut b, else_);
        let wb = fr.work - start;
        fr.work = start.saturating_add(wa.max(wb));
        match (fa, fb) {
            (x, y) if x == y => {
                a.join(&b);
                *st = a;
                x
            }
            (Flow::Normal, _) => {
                *st = a;
                Flow::Normal
            }
            (_, Flow::Normal) => {
                *st = b;
                Flow::Normal
            }
            // Each side leaves, differently: what follows doesn't run in this pass.
            _ => {
                a.join(&b);
                *st = a;
                Flow::Continue
            }
        }
    }

    fn lp(&mut self, fr: &mut Frame, st: &mut State, body: &[Stmt], continuing: &[Stmt]) -> Flow {
        let at = fr.here;
        let mut passes = 0u32;
        // (Counting its calls' runs, a loop goes on past an unbounded call in it: its passes
        // are still sure. Unbounded itself, each call in it counts one pass, and met again, it
        // runs one.)
        let mark = fr.sites.as_ref().map(|s| s.log.len());
        let body_at = body.as_ptr() as usize;
        let endless = fr.sites.as_ref().is_some_and(|s| s.endless.contains(&body_at));
        let mut ran = 0u64;
        loop {
            let done = if mark.is_some() {
                passes >= MOST_PASSES || (endless && passes >= 1)
            } else {
                passes >= MOST_PASSES || fr.unbounded
            };
            if done {
                fr.unbounded = true;
                if fr.loop_at.is_none() {
                    fr.loop_at = at;
                }
                if let (Some(sites), Some(mark)) = (fr.sites.as_mut(), mark)
                    && !endless
                {
                    sites.one_pass(mark, ran);
                    sites.endless.insert(body_at);
                }
                return Flow::Normal;
            }
            passes += 1;
            ran += 1;
            let before = st.locals.clone();
            match self.block(fr, st, body) {
                Flow::Break => {
                    let most = self.passes.entry(body.as_ptr() as usize).or_default();
                    *most = (*most).max(passes);
                    return Flow::Normal;
                }
                Flow::Return => return Flow::Return,
                Flow::Normal | Flow::Continue => {}
            }
            if self.block(fr, st, continuing) == Flow::Return {
                return Flow::Return;
            }
            // A pass that ends where it began runs the same way forever: no exit is sure.
            if st.locals == before {
                passes = MOST_PASSES;
            }
        }
    }

    /// Counts `e`'s work, and gives its value when the count knows it.
    fn expr(&mut self, fr: &mut Frame, st: &mut State, e: &Expr) -> Option<Const> {
        let m = self.m;
        let comps = |v: &ValueId| comps(&m.types, fr.f.value_ty(*v));
        let known = |v: &ValueId| fr.vals.get(v).cloned();
        let (w, value): (u64, Option<Const>) = match e {
            Expr::Const(c) => (0, Some(c.clone())),
            Expr::Zero(_)
            | Expr::Extract(..)
            | Expr::Splat(..)
            | Expr::Swizzle(..)
            | Expr::Construct(..)
            | Expr::Variant(..)
            | Expr::Bitcast(..)
            | Expr::Run(_)
            | Expr::Addr(_)
            | Expr::EntryInput(_)
            | Expr::Discard
            | Expr::Host(..)
            | Expr::Mem(..) => (0, None),
            Expr::Param(i) => (0, fr.args.get(*i as usize).cloned().flatten()),
            Expr::Load(p) => {
                let v = match (&p.root, p.path.is_empty()) {
                    (PlaceRoot::Local(l), true) => st.locals.get(l).cloned(),
                    _ => None,
                };
                (self.place_cost(p, false), v)
            }
            Expr::Unary(op, x) => {
                let v = match (op, known(x)) {
                    (UnOp::Not, Some(Const::Bool(b))) => Some(Const::Bool(!b)),
                    (UnOp::Neg, Some(Const::I32(i))) => Some(Const::I32(i.wrapping_neg())),
                    _ => None,
                };
                (OP * comps(x), v)
            }
            Expr::Binary(op, a, b) => {
                let n = binary_cost(&m.types, fr.f, *op, *a, *b);
                let v = match (known(a), known(b)) {
                    (Some(x), Some(y)) => fold(*op, x, y),
                    _ => None,
                };
                (n, v)
            }
            Expr::Builtin(b, args) => (builtin_cost(*b, args.first().map_or(1, comps)), None),
            Expr::ExtractDyn(..) | Expr::ArrayLength(_) => (OP, None),
            Expr::Convert(x, _) => (OP * comps(x), None),
            Expr::Select { cond, if_true, if_false } => {
                let v = match known(cond) {
                    Some(Const::Bool(true)) => known(if_true),
                    Some(Const::Bool(false)) => known(if_false),
                    _ => None,
                };
                (OP * comps(if_true), v)
            }
            Expr::Texture(op, ..) => (texture_cost(*op), None),
            Expr::TextureStore(..) => (TEXEL, None),
            Expr::Atomic(..) => (ATOMIC, None),
            Expr::Subgroup(..) => (2 * OP, None),
            Expr::Barrier => (4 * OP, None),
            Expr::Call(id, args) => {
                let vals: Vec<Option<Const>> = args
                    .iter()
                    .map(|a| match a {
                        Arg::Value(v) => known(v),
                        Arg::Place(_) => None,
                    })
                    .collect();
                // A place lent to the call may be written there.
                for a in args {
                    if let Arg::Place(p) = a
                        && let PlaceRoot::Local(l) = p.root
                    {
                        st.locals.remove(&l);
                    }
                }
                let c = self.call(*id, &vals);
                if c.unbounded {
                    fr.unbounded = true;
                    if fr.loop_at.is_none() {
                        fr.loop_at = c.loop_at;
                    }
                }
                if let Some(sites) = fr.sites.as_mut() {
                    sites.record(e as *const Expr as usize, known_of(&vals), c.work, c.unbounded);
                }
                let e = fr.calls.entry(id.0).or_insert(0);
                *e = e.saturating_add(c.work);
                (c.work, c.ret)
            }
        };
        fr.work = fr.work.saturating_add(w);
        value
    }

    /// A read (or a write) of `p`: free for a local or a parameter, which are registers.
    fn place_cost(&self, p: &Place, write: bool) -> u64 {
        let index = p.path.iter().any(|x| matches!(x, Proj::Index(_)));
        let base = match &p.root {
            PlaceRoot::Resource(r) => match self.m.resources[r.0 as usize].kind {
                ResourceKind::Uniform { .. } if !write => OP,
                ResourceKind::Workgroup => SHARED,
                ResourceKind::Private => 0,
                _ => MEMORY,
            },
            _ => 0,
        };
        base + if index { OP } else { 0 }
    }
}

/// How many components a value of type `t` has: what elementwise arithmetic on it costs.
fn comps(types: &Types, t: TypeId) -> u64 {
    match types.get(t) {
        TypeDef::Vector(_, n) => u64::from(*n),
        TypeDef::Matrix(c, r) => u64::from(*c) * u64::from(*r),
        _ => 1,
    }
}

fn binary_cost(types: &Types, f: &Function, op: BinOp, a: ValueId, b: ValueId) -> u64 {
    let (ta, tb) = (types.get(f.value_ty(a)), types.get(f.value_ty(b)));
    let per = if matches!(op, BinOp::Div | BinOp::Rem) { SLOW_OP } else { OP };
    let n = match (ta, tb) {
        // A matrix product: a multiply-add for each element of the result, each a row's length.
        (TypeDef::Matrix(c, r), TypeDef::Vector(..)) if op == BinOp::Mul => {
            u64::from(*c) * u64::from(*r)
        }
        (TypeDef::Vector(..), TypeDef::Matrix(c, r)) if op == BinOp::Mul => {
            u64::from(*c) * u64::from(*r)
        }
        (TypeDef::Matrix(k, r), TypeDef::Matrix(c, _)) if op == BinOp::Mul => {
            u64::from(*k) * u64::from(*r) * u64::from(*c)
        }
        _ => comps(types, f.value_ty(a)).max(comps(types, f.value_ty(b))),
    };
    per * n
}

/// A built-in's work, given its first argument's components.
pub fn builtin_cost(b: Builtin, n: u64) -> u64 {
    use Builtin as B;
    match b {
        B::Sqrt
        | B::InverseSqrt
        | B::Sin
        | B::Cos
        | B::Tan
        | B::Asin
        | B::Acos
        | B::Atan
        | B::Atan2
        | B::Exp
        | B::Exp2
        | B::Log
        | B::Log2
        | B::Sinh
        | B::Cosh
        | B::Tanh
        | B::Smoothstep => SLOW_OP * n,
        B::Pow => 2 * SLOW_OP * n,
        B::Clamp | B::Mix | B::Dpdx | B::Dpdy | B::Fwidth => 2 * OP * n,
        B::Dot | B::AllEqual => OP * n,
        B::Length | B::Distance => OP * n + SLOW_OP,
        B::Normalize => 2 * OP * n + SLOW_OP,
        B::Cross => 6 * OP,
        _ => OP * n,
    }
}

fn texture_cost(op: TextureOp) -> u64 {
    match op {
        TextureOp::Load => TEXEL,
        TextureOp::Width
        | TextureOp::Height
        | TextureOp::Depth
        | TextureOp::Layers
        | TextureOp::Levels => OP,
        _ => SAMPLE,
    }
}

/// An integer or `bool` operation on known values, as the GPU computes it.
fn fold(op: BinOp, a: Const, b: Const) -> Option<Const> {
    use std::cmp::Ordering;
    let cmp = |o: Ordering| {
        Some(Const::Bool(match op {
            BinOp::Eq => o == Ordering::Equal,
            BinOp::Ne => o != Ordering::Equal,
            BinOp::Lt => o == Ordering::Less,
            BinOp::Le => o != Ordering::Greater,
            BinOp::Gt => o == Ordering::Greater,
            BinOp::Ge => o != Ordering::Less,
            _ => return None,
        }))
    };
    match (a, b) {
        (Const::I32(x), Const::I32(y)) => match op {
            BinOp::Add | BinOp::WrappingAdd => Some(Const::I32(x.wrapping_add(y))),
            BinOp::Sub | BinOp::WrappingSub => Some(Const::I32(x.wrapping_sub(y))),
            BinOp::Mul | BinOp::WrappingMul => Some(Const::I32(x.wrapping_mul(y))),
            BinOp::Div if y != 0 => Some(Const::I32(x.wrapping_div(y))),
            BinOp::Rem if y != 0 => Some(Const::I32(x.wrapping_rem(y))),
            _ => cmp(x.cmp(&y)),
        },
        (Const::U32(x), Const::U32(y)) => match op {
            BinOp::Add | BinOp::WrappingAdd => Some(Const::U32(x.wrapping_add(y))),
            BinOp::Sub | BinOp::WrappingSub => Some(Const::U32(x.wrapping_sub(y))),
            BinOp::Mul | BinOp::WrappingMul => Some(Const::U32(x.wrapping_mul(y))),
            BinOp::Div if y != 0 => Some(Const::U32(x / y)),
            BinOp::Rem if y != 0 => Some(Const::U32(x % y)),
            BinOp::BitAnd => Some(Const::U32(x & y)),
            BinOp::BitOr => Some(Const::U32(x | y)),
            BinOp::Shl => Some(Const::U32(x.wrapping_shl(y))),
            BinOp::Shr => Some(Const::U32(x.wrapping_shr(y))),
            _ => cmp(x.cmp(&y)),
        },
        (Const::Bool(x), Const::Bool(y)) => match op {
            BinOp::And => Some(Const::Bool(x && y)),
            BinOp::Or => Some(Const::Bool(x || y)),
            BinOp::Eq => Some(Const::Bool(x == y)),
            BinOp::Ne => Some(Const::Bool(x != y)),
            _ => None,
        },
        _ => None,
    }
}

/// How many statements function `f` comes to with every call written out in place: what a
/// GPU's shader compiler works through, since it copies a function into each place it's called
/// (§18.11, a pipeline's weight). A big function called from many places weighs each time.
pub fn weight(m: &Module, f: FuncId) -> u64 {
    fn go(m: &Module, f: FuncId, memo: &mut HashMap<u32, u64>) -> u64 {
        if let Some(&w) = memo.get(&f.0) {
            return w;
        }
        // A recursion (which GPU code can't have) counts once.
        memo.insert(f.0, 1);
        let mut own = 0u64;
        let mut calls = Vec::new();
        visit::walk(&m.functions[f.0 as usize].body, &mut |s| {
            if !matches!(s, Stmt::At(_)) {
                own += 1;
            }
            if let Some(Expr::Call(id, _)) = s.expr() {
                calls.push(*id);
            }
        });
        let w = calls.into_iter().fold(own, |w, c| w.saturating_add(go(m, c, memo)));
        memo.insert(f.0, w);
        w
    }
    go(m, f, &mut HashMap::new())
}
