//! Effects (language.md §8): what a function may do besides compute its result. There are six:
//! `alloc` (the heap), `io`, `nondet` (anything whose result isn't a function of the inputs),
//! `recursion` (a cycle in the call graph), `host` (recording GPU work), and `panic`. They're
//! inferred from each body and everything it calls, never written, and checked against the
//! contexts that forbid some: GPU entry points and `@gpu` functions, `@audio`, `@deterministic`,
//! the functions a derived interpretation derives, and build-time constants.
//!
//! The analysis runs on every body the checker built, whether or not anything calls it, so
//! effect errors come with the other errors. A call through a trait method on a generic
//! parameter, or through a `fn(..)` parameter, depends on what's instantiated: lowering checks
//! those per instantiation. A closure's effects are its maker's (it runs when it's called, and
//! it's called by the code it's passed to on the maker's behalf).

use crate::defs::*;
use crate::mir::{self, Body, Callee, Rvalue, StatementKind, TerminatorKind};
use crate::program::Program;
use crate::traits;
use crate::ty::*;
use std::collections::{BTreeMap, HashMap, VecDeque};
use wrela_diag::{Diagnostic, Span, codes};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Effect {
    Alloc,
    Io,
    Nondet,
    Recursion,
    Host,
    Panic,
}

impl Effect {
    pub const ALL: [Effect; 6] =
        [Effect::Alloc, Effect::Io, Effect::Nondet, Effect::Recursion, Effect::Host, Effect::Panic];

    /// The effect `@effects(...)` names `name`.
    pub fn named(name: &str) -> Option<Effect> {
        Effect::ALL.into_iter().find(|e| e.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            Effect::Alloc => "alloc",
            Effect::Io => "io",
            Effect::Nondet => "nondet",
            Effect::Recursion => "recursion",
            Effect::Host => "host",
            Effect::Panic => "panic",
        }
    }

    /// What doing it is, as a verb phrase: `allocates`.
    fn verb(self) -> &'static str {
        match self {
            Effect::Alloc => "allocates",
            Effect::Io => "does IO",
            Effect::Nondet => "is non-deterministic",
            Effect::Recursion => "recurses",
            Effect::Host => "records GPU work",
            Effect::Panic => "can panic",
        }
    }
}

/// A body: a function's own, or one of its closures'.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BodyId {
    pub func: FnId,
    pub closure: Option<ClosureId>,
}

/// One body's own effects (where each happens) and what it calls (where).
#[derive(Default, Debug)]
struct Node {
    direct: Vec<(Effect, Span, String)>,
    calls: Vec<(BodyId, Span)>,
    /// It calls something whose effects depend on what's instantiated (a trait method of a
    /// generic parameter, a closure or a function passed in): lowering knows them, not this.
    open: bool,
}

/// The effect graph of a program's bodies.
pub struct Effects<'p> {
    p: &'p Program,
    nodes: BTreeMap<BodyId, Node>,
    /// Each body's effects, everything it reaches included.
    all: HashMap<BodyId, Vec<Effect>>,
    /// The bodies that reach a call whose effects aren't known here ([`Node::open`]).
    open: std::collections::HashSet<BodyId>,
}

/// Checks every context's effects (E0600).
pub fn check(p: &Program, mir: &BTreeMap<FnId, Body>) -> Vec<Diagnostic> {
    let fx = Effects::build(p, mir);
    let mut out = Vec::new();
    for (&f, body) in mir {
        let def = p.func(f);
        let root = BodyId { func: f, closure: None };
        // Each context: what it forbids, and how to say what it is.
        let mut contexts: Vec<(&[Effect], String)> = Vec::new();
        if let Some((entry, _)) = def.attrs.entry {
            let what = match entry {
                Entry::Compute(_) => "a compute kernel",
                Entry::Vertex => "a vertex shader",
                Entry::Fragment => "a fragment shader",
            };
            contexts.push((GPU, format!("`{}` is {what}, GPU code", def.name)));
        } else if def.attrs.gpu.is_some() {
            contexts.push((GPU, format!("`{}` is declared `@gpu`", def.name)));
        }
        if def.attrs.audio.is_some() {
            contexts.push((AUDIO, format!("`{}` is `@audio` code, on the audio thread", def.name)));
        }
        if def.attrs.deterministic.is_some() {
            contexts.push((DETERMINISTIC, format!("`{}` is `@deterministic`", def.name)));
        }
        if let FnOwner::Const(_) = def.owner {
            contexts.push((
                DETERMINISTIC,
                format!("`{}` is a constant, which the build computes", def.name),
            ));
        }
        if def.attrs.test.is_some() {
            contexts
                .push((DETERMINISTIC, format!("`{}` is a test, which the build runs", def.name)));
        }
        for (forbids, ctx) in contexts {
            out.extend(fx.report(root, forbids, &ctx, def.sig_span, codes::E0600));
        }
        for (callee, span) in derived_callables(p, f, body) {
            let ctx = "a derived interpretation derives it";
            out.extend(fx.report(callee, DERIVED, ctx, span, codes::E0700));
        }
        out.extend(discarded_results(p, body, &fx));
        // What's passed as a `@deterministic fn` or a `@parallel fn` (§9, §6.12).
        for arg in typed_callables(p, f, body) {
            if arg.flags.deterministic {
                let ctx = format!("`{}` takes it as a `@deterministic fn`", arg.callee);
                out.extend(fx.report(arg.body, DETERMINISTIC, &ctx, arg.span, codes::E0600));
            }
            if arg.flags.parallel {
                let ctx = format!("`{}` runs it on several workers at once", arg.callee);
                out.extend(fx.report(arg.body, DETERMINISTIC, &ctx, arg.span, codes::E0600));
                out.extend(parallel_writes(body, &arg));
            }
            if arg.flags.audio {
                let ctx = format!("`{}` runs it on the audio thread", arg.callee);
                out.extend(fx.report(arg.body, AUDIO, &ctx, arg.span, codes::E0600));
                out.extend(audio_captures(body, &arg));
            }
        }
    }
    out
}

/// E0333: a statement that calls a function only to throw its value away, where the call
/// does nothing else: no effect, and no argument lent `mut`. `p.pos.normalize()` or
/// `grow(p, 2.0)` written as statements look like changes in place to a reader used to them,
/// and do nothing. `let _ = f()` discards on purpose.
fn discarded_results(p: &Program, body: &Body, fx: &Effects) -> Vec<Diagnostic> {
    use crate::builtins::BuiltinFn;
    let mut out = Vec::new();
    for (callee, lends_mut, ty, span) in &body.discarded {
        if *lends_mut
            || p.types.has_params(*ty)
            || p.types.is_unit(*ty)
            || matches!(p.types.kind(*ty), TyKind::Never | TyKind::Error)
        {
            continue;
        }
        let name = match callee {
            Callee::Builtin(BuiltinFn::Panic | BuiltinFn::Assert) => continue,
            Callee::Builtin(b) => b.name().to_string(),
            Callee::Fn { func, .. } if fx.pure(*func) => p.func(*func).name.clone(),
            _ => continue,
        };
        out.push(
            Diagnostic::new(
                codes::E0333,
                *span,
                format!("this throws away what `{name}` returns, and the call does nothing else"),
            )
            .with_note("a call changes only what it's lent `mut`; this one is lent nothing, so its result is all it gives")
            .with_help("use the result (`x = x.normalize()`), or write `let _ = ...` to discard it on purpose")
            .with_fix("discard it on purpose", span.shrink_to_start(), "let _ = "),
        );
    }
    out
}

/// A function or closure passed where a function type with attributes is expected.
struct TypedArg {
    body: BodyId,
    /// The argument's span.
    span: Span,
    flags: FnFlags,
    /// The function it's passed to.
    callee: String,
}

/// The closures and named functions a body passes to `@deterministic fn` and `@parallel fn`
/// parameters.
fn typed_callables(p: &Program, f: FnId, body: &Body) -> Vec<TypedArg> {
    let mut out = Vec::new();
    calls_with_held(f, body, |c, held| {
        let callee = match &c.callee {
            Callee::Fn { func, .. } => *func,
            Callee::TraitMethod { method, .. } => *method,
            _ => return,
        };
        let def = p.func(callee);
        for (param, a) in def.params.iter().zip(&c.args) {
            // A function type, or a generic parameter bounded by one (`F: @deterministic
            // fn(mut S, Ticked)`, which keeps the function it's given).
            let fn_ty = match p.types.kind(param.ty) {
                TyKind::Param(g) => p.param(*g).fn_bound.unwrap_or(param.ty),
                _ => param.ty,
            };
            let TyKind::FnPtr(_, _, flags) = p.types.kind(fn_ty) else { continue };
            if !(flags.deterministic || flags.parallel || flags.audio) {
                continue;
            }
            let Some(&target) = a.place().and_then(|pl| held.get(&pl.local)) else {
                continue;
            };
            out.push(TypedArg {
                body: target,
                span: a.span(),
                flags: *flags,
                callee: def.name.clone(),
            });
        }
    });
    out
}

/// Calls `visit` on each call in `body` (in its closures too), with what each local holds at
/// that point: a closure or a named function.
fn calls_with_held(
    f: FnId,
    body: &Body,
    mut visit: impl FnMut(&mir::Call, &HashMap<mir::Local, BodyId>),
) {
    let mut held: HashMap<mir::Local, BodyId> = HashMap::new();
    for code in &body.fns {
        for b in &code.blocks {
            for s in &b.stmts {
                let (StatementKind::Assign(_, r) | StatementKind::Eval(r)) = &s.kind else {
                    continue;
                };
                if let StatementKind::Assign(place, r) = &s.kind
                    && place.proj.is_empty()
                {
                    match r {
                        Rvalue::Closure(id) => {
                            held.insert(place.local, BodyId { func: f, closure: Some(*id) });
                        }
                        Rvalue::FnRef(g, _) => {
                            held.insert(place.local, BodyId { func: *g, closure: None });
                        }
                        _ => {}
                    }
                }
                if let Rvalue::Call(c) = r {
                    visit(c, &held);
                }
            }
        }
    }
}

/// A closure passed as an `@audio fn` that captures anything: the audio thread keeps it
/// after the call that passed it returns, so its voice's data is its state instead (E0510).
fn audio_captures(body: &Body, arg: &TypedArg) -> Vec<Diagnostic> {
    let Some(c) = arg.body.closure else { return Vec::new() };
    let info = &body.closures[c.0 as usize];
    let Some((first, _)) = info.captures.first() else { return Vec::new() };
    vec![
        Diagnostic::new(
            codes::E0510,
            info.span,
            format!(
                "this closure captures `{}`, but `{}` keeps it on the audio thread after this \
                 call returns",
                body.local(*first).name,
                arg.callee
            ),
        )
        .with_help("pass what the voice needs in its state, the argument after it, or pass a named function"),
    ]
}

/// A closure passed as a `@parallel fn` that writes data it captures: several workers would
/// write it at once (E0520).
fn parallel_writes(body: &Body, arg: &TypedArg) -> Vec<Diagnostic> {
    let Some(c) = arg.body.closure else { return Vec::new() };
    let info = &body.closures[c.0 as usize];
    let written: Vec<&str> = info
        .captures
        .iter()
        .filter(|(_, writes)| *writes)
        .map(|(l, _)| body.local(*l).name.as_str())
        .collect();
    let Some(first) = written.first() else { return Vec::new() };
    let names: Vec<String> = written.iter().map(|n| format!("`{n}`")).collect();
    vec![
        Diagnostic::new(
            codes::E0520,
            info.span,
            format!(
                "this closure writes {}, which it captures, and `{}` runs it on several workers \
                 at once",
                names.join(" and "),
                arg.callee
            ),
        )
        .with_note(format!(
            "workers that write `{first}` at the same time would race; a parallel closure writes \
             only its own element (§6.12)"
        ))
        .with_note(
            "to combine a value from each element, compute it in the closure and combine them \
             with `par_map_reduce`",
        ),
    ]
}

// What each context forbids.
const GPU: &[Effect] =
    &[Effect::Alloc, Effect::Io, Effect::Nondet, Effect::Recursion, Effect::Host, Effect::Panic];
const AUDIO: &[Effect] = &[Effect::Io, Effect::Host, Effect::Recursion, Effect::Alloc];
/// `@deterministic` code, a constant, and what runs on several workers at once. A replayed tick
/// mustn't touch storage either: requests are `io` (§6.15).
const DETERMINISTIC: &[Effect] = &[Effect::Io, Effect::Nondet, Effect::Host];
/// What's derived can't allocate, do IO, be non-deterministic or call the host (§13).
const DERIVED: &[Effect] = &[Effect::Alloc, Effect::Io, Effect::Nondet, Effect::Host];

/// The bodies a function hands to `gradient`, `value_and_gradient` or `interval`, and where.
fn derived_callables(p: &Program, f: FnId, body: &Body) -> Vec<(BodyId, Span)> {
    let mut out = Vec::new();
    calls_with_held(f, body, |c, held| {
        let Callee::Fn { func, .. } = &c.callee else { return };
        let derived = matches!(
            p.func(*func).lang,
            Some(
                Lang::Gradient
                    | Lang::ValueAndGradient
                    | Lang::ValueGradientWith
                    | Lang::IntervalOf
                    | Lang::LiftGradient,
            )
        );
        if derived
            && let Some(place) = c.args.first().and_then(|a| a.place())
            && let Some(&target) = held.get(&place.local)
        {
            out.push((target, c.span));
        }
    });
    out
}

/// One effect of a function's, for tools (`wrela query`): a shortest chain of calls to where
/// it happens (the function itself first), and where and what it is there.
#[derive(Clone, Debug)]
pub struct EffectWhy {
    pub effect: Effect,
    pub chain: Vec<String>,
    pub span: Span,
    pub what: String,
}

impl<'p> Effects<'p> {
    /// The effect graph of every body in `mir`.
    pub fn build(p: &'p Program, mir: &BTreeMap<FnId, Body>) -> Effects<'p> {
        let hidden: HashMap<FnId, TyId> =
            mir.iter().filter_map(|(&f, b)| b.hidden_ret.map(|h| (f, h))).collect();
        let mut nodes = BTreeMap::new();
        for (&f, body) in mir {
            for (k, code) in body.fns.iter().enumerate() {
                let closure = (k > 0).then(|| ClosureId(k as u32 - 1));
                let id = BodyId { func: f, closure };
                nodes.insert(id, node(p, f, body, code, &hidden));
            }
        }
        // std functions without bodies here (intrinsics) have their effects by what they are.
        let mut fx = Effects { p, nodes, all: HashMap::new(), open: Default::default() };
        fx.mark_recursion();
        fx.propagate();
        fx
    }

    /// Functions on a cycle of calls get `recursion`, where the cycle closes.
    fn mark_recursion(&mut self) {
        let ids: Vec<BodyId> = self.nodes.keys().copied().collect();
        let index: HashMap<BodyId, usize> = ids.iter().enumerate().map(|(i, &b)| (b, i)).collect();
        let adj: Vec<Vec<usize>> = ids
            .iter()
            .map(|b| {
                self.nodes[b].calls.iter().filter_map(|(c, _)| index.get(c).copied()).collect()
            })
            .collect();
        for members in sccs(&adj) {
            let cyclic = members.len() > 1 || adj[members[0]].contains(&members[0]);
            if !cyclic {
                continue;
            }
            for &m in &members {
                let b = ids[m];
                // The call that stays on the cycle.
                let call = self.nodes[&b]
                    .calls
                    .iter()
                    .find(|(c, _)| index.get(c).is_some_and(|i| members.contains(i)))
                    .cloned();
                if let Some((callee, span)) = call {
                    let name = self.name(callee);
                    self.nodes.get_mut(&b).expect("a node").direct.push((
                        Effect::Recursion,
                        span,
                        format!("calls `{name}`, which leads back here"),
                    ));
                }
            }
        }
    }

    fn propagate(&mut self) {
        // Iterate to a fixpoint: effect sets only grow, and there are six. What reaches an open
        // call is open too: a call to a body that isn't here (an intrinsic) isn't.
        let ids: Vec<BodyId> = self.nodes.keys().copied().collect();
        for &b in &ids {
            let mut e: Vec<Effect> = self.nodes[&b].direct.iter().map(|d| d.0).collect();
            e.sort();
            e.dedup();
            self.all.insert(b, e);
        }
        self.open = ids.iter().copied().filter(|b| self.nodes[b].open).collect();
        loop {
            let mut changed = false;
            for &b in &ids {
                let mut e = self.all[&b].clone();
                let mut open = false;
                for (c, _) in &self.nodes[&b].calls {
                    if let Some(ce) = self.all.get(c) {
                        e.extend(ce.iter().copied());
                    }
                    open |= self.open.contains(c);
                }
                e.sort();
                e.dedup();
                if e != self.all[&b] {
                    self.all.insert(b, e);
                    changed = true;
                }
                if open && self.open.insert(b) {
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// `f`'s effects, each with why it has it, and whether it calls something whose effects
    /// depend on how it's instantiated (lowering checks those). `None` if `f` has no body here.
    pub fn explain(&self, f: FnId) -> Option<(Vec<EffectWhy>, bool)> {
        let root = BodyId { func: f, closure: None };
        let has = self.all.get(&root)?;
        let mut out = Vec::new();
        for &e in has {
            if let Some((chain, span, what)) = self.witness(root, e) {
                let chain = chain.iter().map(|&b| self.name(b)).collect();
                out.push(EffectWhy { effect: e, chain, span, what });
            }
        }
        Some((out, self.open.contains(&root)))
    }

    /// Whether calling `f` does nothing but compute its result: no effect, through everything
    /// it calls, and nothing whose effects depend on its instantiation.
    fn pure(&self, f: FnId) -> bool {
        let b = BodyId { func: f, closure: None };
        self.all.get(&b).is_some_and(Vec::is_empty) && !self.open.contains(&b)
    }

    fn name(&self, b: BodyId) -> String {
        match b.closure {
            None => self.p.fn_display_name(b.func),
            Some(_) => format!("a closure in `{}`", self.p.fn_display_name(b.func)),
        }
    }

    /// E0600 for each effect in `forbids` that `root` has, with the call chain to where it
    /// happens. `at` is where the context is (an entry point's signature).
    fn report(
        &self,
        root: BodyId,
        forbids: &[Effect],
        ctx: &str,
        at: Span,
        code: wrela_diag::Code,
    ) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let Some(has) = self.all.get(&root) else { return out };
        for &e in forbids {
            if !has.contains(&e) {
                continue;
            }
            let Some((chain, span, what)) = self.witness(root, e) else { continue };
            // The chain stops at std: what std does inside is std's business.
            let mut names: Vec<String> = Vec::new();
            for &b in &chain {
                names.push(self.name(b));
                if self.p.is_std(self.p.func(b.func).module) {
                    break;
                }
            }
            let last = names.last().cloned().unwrap_or_default();
            // The error is where it happens in the program's own code: the effect itself, or
            // the call into std that does it.
            let first_std = chain.iter().position(|b| self.p.is_std(self.p.func(b.func).module));
            let (primary, here) = match first_std {
                Some(0) => (at, format!("`{last}` {}", e.verb())),
                Some(j) => {
                    let call = self.nodes[&chain[j - 1]]
                        .calls
                        .iter()
                        .find(|(c, _)| *c == chain[j])
                        .map_or(span, |(_, s)| *s);
                    (call, format!("`{last}` {}", e.verb()))
                }
                None => (span, format!("this {what}")),
            };
            let mut d = Diagnostic::new(
                code,
                primary,
                format!("{here}, and {ctx}, which can't `{}`", e.name()),
            );
            if names.len() > 1 {
                d = d.with_note(format!("the call chain: {}", names.join(" → ")));
            }
            d = d.with_note(format!(
                "{} is the `{}` effect; see language.md §8 for what each context forbids",
                match e {
                    Effect::Alloc => "using the heap (a `Vec`, a `String`, a `Box`, an f-string)",
                    Effect::Io => "reading or writing outside the program",
                    Effect::Nondet => "depending on something besides the inputs (the clock, randomness, readback)",
                    Effect::Recursion => "a cycle of calls",
                    Effect::Host => "recording GPU work",
                    Effect::Panic => "a panic, a failed `assert`, or a `Vec` index past its end",
                },
                e.name()
            ));
            out.push(d);
        }
        out
    }

    /// A shortest chain of calls from `root` to a body where `e` happens directly: the bodies,
    /// and where and what the effect is there.
    /// A drop is the witness only when nothing else is: what made the value is the clearer
    /// cause (an f-string allocates, and its drop frees).
    fn witness(&self, root: BodyId, e: Effect) -> Option<(Vec<BodyId>, Span, String)> {
        let mut prev: HashMap<BodyId, BodyId> = HashMap::new();
        let mut queue = VecDeque::from([root]);
        let mut seen = std::collections::HashSet::from([root]);
        let mut drop = None;
        while let Some(b) = queue.pop_front() {
            let node = self.nodes.get(&b)?;
            let found = node.direct.iter().filter(|d| d.0 == e);
            let mut found: Vec<_> = found.collect();
            found.sort_by_key(|d| d.2.contains("drop"));
            if let Some((_, span, what)) = found.first() {
                let mut chain = vec![b];
                let mut at = b;
                while let Some(&p) = prev.get(&at) {
                    chain.push(p);
                    at = p;
                }
                chain.reverse();
                let w = (chain, *span, what.clone());
                if !what.contains("drop") {
                    return Some(w);
                }
                drop.get_or_insert(w);
            }
            for (c, _) in &node.calls {
                if self.all.get(c).is_some_and(|ce| ce.contains(&e)) && seen.insert(*c) {
                    prev.insert(*c, b);
                    queue.push_back(*c);
                }
            }
        }
        drop
    }
}

/// A body's own effects and calls.
fn node(
    p: &Program,
    f: FnId,
    body: &Body,
    code: &mir::FnBody,
    hidden: &HashMap<FnId, TyId>,
) -> Node {
    let mut n = Node::default();
    let std_module = |g: FnId| p.module(p.func(g).module).path.get(1).cloned();
    let call_to = |n: &mut Node, g: FnId, span: Span| {
        let def = p.func(g);
        // What std's core states it does, which inference can't see (`@effects`): at the call
        // when it has no body here, else where its body is (below).
        if def.attrs.intrinsic {
            for &e in &def.attrs.effects {
                n.direct.push((e, span, format!("`{}` is declared `{}`", def.name, e.name())));
            }
        }
        // std's intrinsics and its heap: the effects are what they are.
        if def.lang.is_some_and(Lang::is_nondet) {
            let why = format!("`{}` depends on when the host answers", def.name);
            n.direct.push((Effect::Nondet, span, why));
        }
        if def.lang.is_some_and(Lang::requests_io) {
            n.direct.push((Effect::Io, span, format!("`{}` asks the host for IO", def.name)));
            return;
        }
        if def.lang == Some(Lang::LiftSet) {
            n.direct.push((Effect::Host, span, "`set` changes a lifted literal".into()));
            return;
        }
        if def.lang.is_some_and(Lang::records_gpu_work) {
            n.direct.push((Effect::Host, span, format!("`{}` records GPU work", def.name)));
            return;
        }
        if def.lang.is_some_and(Lang::is_nondet) {
            return;
        }
        // (Reading the allocator's count isn't allocating.)
        if p.is_std(def.module)
            && std_module(g).as_deref() == Some("alloc")
            && def.name != "allocations"
        {
            n.direct.push((Effect::Alloc, span, "call uses the heap".into()));
            return;
        }
        n.calls.push((BodyId { func: g, closure: None }, span));
    };
    if std::ptr::eq(code, &body.fns[0]) {
        let def = p.func(f);
        for &e in &def.attrs.effects {
            let why = format!("`{}` is declared `{}`", def.name, e.name());
            n.direct.push((e, def.sig_span, why));
        }
    }
    for b in &code.blocks {
        for s in &b.stmts {
            let r = match &s.kind {
                StatementKind::Assign(place, r) => {
                    index_panics(p, body, place, s.span, &mut n);
                    r
                }
                StatementKind::Eval(r) => r,
                StatementKind::Drop(place) => {
                    // Through the types functions return by naming their traits: dropping
                    // `wolf()`'s result drops what `wolf` made, which may own nothing.
                    let t = traits::reveal(p, hidden, mir::place_ty(p, &body.locals, place));
                    if heap_type(p, t) {
                        let (heap, gpu) = drop_effects(p, t);
                        if gpu {
                            let why = "drop releases a GPU buffer".into();
                            n.direct.push((Effect::Host, s.span, why));
                        }
                        if heap {
                            n.direct.push((Effect::Alloc, s.span, "drop frees heap memory".into()));
                        }
                    }
                    continue;
                }
                StatementKind::Bind { place, .. } | StatementKind::Check(place) => {
                    index_panics(p, body, place, s.span, &mut n);
                    continue;
                }
                _ => continue,
            };
            match r {
                Rvalue::Call(c) => match &c.callee {
                    Callee::Fn { func, .. } => call_to(&mut n, *func, c.span),
                    // Resuming a job runs its body to its next `yield`; starting one moves its
                    // arguments into its value.
                    Callee::JobResume(func) => call_to(&mut n, *func, c.span),
                    Callee::JobStart(_) => {}
                    Callee::TraitMethod { method, self_ty, trait_args, method_args } => {
                        // Resolved here only when the type is known; otherwise lowering checks
                        // each instantiation.
                        if !p.types.has_params(*self_ty)
                            && let Some((g, _)) = traits::resolve_trait_method(
                                p,
                                *method,
                                *self_ty,
                                trait_args,
                                method_args,
                            )
                        {
                            call_to(&mut n, g, c.span);
                        } else {
                            n.open = true;
                        }
                    }
                    Callee::Builtin(crate::builtins::BuiltinFn::Panic) => {
                        n.direct.push((Effect::Panic, c.span, "`panic`".into()));
                    }
                    Callee::Builtin(crate::builtins::BuiltinFn::Assert) => {
                        n.direct.push((Effect::Panic, c.span, "`assert`".into()));
                    }
                    Callee::Clone => {
                        if let Some(a) = c.args.first().and_then(|a| a.place()) {
                            let t = mir::place_ty(p, &body.locals, a);
                            if heap_type(p, t) {
                                n.direct.push((
                                    Effect::Alloc,
                                    c.span,
                                    "`.clone()` copies heap memory".into(),
                                ));
                            }
                        }
                    }
                    Callee::Local(_) | Callee::Value(_) => n.open = true,
                    Callee::Builtin(_) => {}
                },
                Rvalue::Dispatch(_) => {
                    n.direct.push((Effect::Host, s.span, "`dispatch` records GPU work".into()));
                }
                Rvalue::Draw(_) => {
                    n.direct.push((Effect::Host, s.span, "`draw` records GPU work".into()));
                }
                Rvalue::Closure(id) => {
                    n.calls.push((BodyId { func: f, closure: Some(*id) }, s.span));
                }
                Rvalue::FnRef(g, _) => call_to(&mut n, *g, s.span),
                Rvalue::Use(o) => {
                    if let Some(place) = o.place() {
                        index_panics(p, body, place, s.span, &mut n);
                    }
                }
                _ => {}
            }
        }
        if let TerminatorKind::ReturnPlace(place) = &b.term.kind {
            index_panics(p, body, place, b.term.span, &mut n);
        }
    }
    n
}

/// A `Vec` or arena index past the end panics (an array's traps instead, §11).
fn index_panics(p: &Program, body: &Body, place: &mir::Place, span: Span, n: &mut Node) {
    let mut t = body.local(place.local).ty;
    let mut variant = None;
    for proj in &place.proj {
        if let mir::Proj::Index(_) = proj
            && matches!(p.lang_of_ty(t), Some(Lang::Vec | Lang::Arena))
        {
            n.direct.push((Effect::Panic, span, "index can be past the end".into()));
            return;
        }
        t = mir::proj_ty(p, t, &mut variant, proj);
    }
}

/// Whether a value of type `t` may own heap memory.
fn heap_type(p: &Program, t: TyId) -> bool {
    traits::may_need_drop(p, t) && !p.types.has_params(t)
}

/// What dropping a value of type `t` does: frees heap memory (`alloc`), releases GPU buffers
/// (`host`), or both. A destructor written in the program counts as `alloc`.
fn drop_effects(p: &Program, t: TyId) -> (bool, bool) {
    fn walk(p: &Program, t: TyId, seen: &mut Vec<TyId>, out: &mut (bool, bool)) {
        if seen.contains(&t) || !traits::may_need_drop(p, t) {
            return;
        }
        seen.push(t);
        match p.types.kind(t) {
            TyKind::Tuple(ts) => ts.iter().for_each(|&e| walk(p, e, seen, out)),
            TyKind::Array(e, _) | TyKind::ArrayN(e, _) => walk(p, *e, seen, out),
            TyKind::Adt(a, args) => {
                let adt = p.adt(*a);
                if adt.lang == Some(Lang::GpuBuffer) {
                    out.1 = true;
                    return;
                }
                out.0 |= traits::explicit_drop(p, *a).is_some();
                for v in p.field_lists(*a) {
                    for f in p.fields_of(*a, args, v) {
                        walk(p, f, seen, out);
                    }
                }
            }
            _ => out.0 = true,
        }
    }
    let mut out = (false, false);
    walk(p, t, &mut Vec::new(), &mut out);
    out
}

/// The strongly connected components of a graph (Tarjan's), each a list of nodes.
pub fn sccs(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    struct T<'a> {
        adj: &'a [Vec<usize>],
        index: Vec<Option<u32>>,
        low: Vec<u32>,
        on: Vec<bool>,
        stack: Vec<usize>,
        next: u32,
        out: Vec<Vec<usize>>,
    }
    impl T<'_> {
        fn visit(&mut self, v: usize) {
            // An explicit stack: call graphs can be deep.
            let mut work: Vec<(usize, usize)> = vec![(v, 0)];
            self.index[v] = Some(self.next);
            self.low[v] = self.next;
            self.next += 1;
            self.stack.push(v);
            self.on[v] = true;
            while let Some(&mut (u, ref mut i)) = work.last_mut() {
                if *i < self.adj[u].len() {
                    let w = self.adj[u][*i];
                    *i += 1;
                    match self.index[w] {
                        None => {
                            self.index[w] = Some(self.next);
                            self.low[w] = self.next;
                            self.next += 1;
                            self.stack.push(w);
                            self.on[w] = true;
                            work.push((w, 0));
                        }
                        Some(wi) if self.on[w] => self.low[u] = self.low[u].min(wi),
                        Some(_) => {}
                    }
                    continue;
                }
                work.pop();
                if let Some(&(parent, _)) = work.last() {
                    self.low[parent] = self.low[parent].min(self.low[u]);
                }
                if Some(self.low[u]) == self.index[u] {
                    let mut comp = Vec::new();
                    while let Some(x) = self.stack.pop() {
                        self.on[x] = false;
                        comp.push(x);
                        if x == u {
                            break;
                        }
                    }
                    self.out.push(comp);
                }
            }
        }
    }
    let n = adj.len();
    let mut t = T {
        adj,
        index: vec![None; n],
        low: vec![0; n],
        on: vec![false; n],
        stack: Vec::new(),
        next: 0,
        out: Vec::new(),
    };
    for v in 0..n {
        if t.index[v].is_none() {
            t.visit(v);
        }
    }
    t.out
}
