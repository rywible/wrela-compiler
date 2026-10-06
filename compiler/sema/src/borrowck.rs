//! The memory checker (language.md §6): moves, projections and exclusivity, as dataflow over a
//! function's MIR. Signatures say everything about callees.
//!
//! - **Places** are a local and a path. Two places overlap when one is a prefix of the other;
//!   every element of a container overlaps every other.
//! - **Moves** flow forward along the control-flow graph: a value moved on some path is
//!   "possibly moved" where the paths join, and a move that reaches a use only around a loop's
//!   back edge is a move inside the loop of a value from outside it (E0515).
//! - **Loans** are held by locals: a projection (`let x = place`, `mut x = place`, a pattern's
//!   binding, a `for` loop's element), a call's argument (until the call), what a
//!   projection-returning call returned (a loan on each of the call's `borrow` and `mut`
//!   arguments), and a closure (its captures). A loan lasts while its holder is live: used
//!   later on some path (a simple non-lexical lifetime). A projection of a projection holds the
//!   original's loans itself, so it can outlive it.
//! - **The rule:** while a `mut` loan is live, no other access may touch an overlapping place;
//!   while a shared loan is live, nothing may write, move or lend mutably an overlapping place.
//!   An access through a projection is that projection's own use, and its loans' places are
//!   what it touches.
//!
//! The rules that depend only on how code is written are checked while the MIR is built
//! ([`crate::mir::build`]).

use crate::defs::{Mode, RetMode};
use crate::mir::*;
use crate::program::Program;
use crate::thir;
use crate::ty::ClosureId;
use std::collections::{BTreeSet, HashMap, HashSet};
use wrela_diag::{Diagnostic, Span, codes};

/// Whether a borrow struct has a `mut` field, in it or in a borrow struct inside it.
pub(crate) fn has_mut_field(p: &Program, t: crate::ty::TyId) -> bool {
    let crate::ty::TyKind::Adt(a, args) = p.types.kind(t) else { return false };
    p.adt_fields(*a, None)
        .iter()
        .zip(p.fields_of(*a, args, None))
        .any(|(f, ft)| f.mode == RetMode::Mut || (p.is_borrow_struct(ft) && has_mut_field(p, ft)))
}

/// Checks every function and closure body of `body`.
pub fn check(p: &Program, body: &Body) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    run_bodies(p, body, None, &mut |diags, _| out.extend(diags));
    out.extend(unused_locals(p, body));
    out.extend(unchanged_vars(body));
    out
}

/// Runs the checker on every function and closure body of `body`, giving `each` what each run
/// finds: its diagnostics, and the state before the line `asked` about ([`state_at`]), if the
/// body has it. A body is checked before the closures it makes, so a closure's body knows what
/// the projections it captures alias.
fn run_bodies(
    p: &Program,
    body: &Body,
    asked: Option<(wrela_diag::FileId, u32, u32)>,
    each: &mut dyn FnMut(Vec<Diagnostic>, Option<(u32, StateView)>),
) {
    let holders: Vec<bool> = body
        .locals
        .iter()
        .map(|d| d.kind.is_projection() || is_callable(&p.types, d.ty) || p.is_borrow_struct(d.ty))
        .collect();
    let mut captured = Captured::new();
    for (i, f) in body.fns.iter().enumerate() {
        let closure = (i > 0).then(|| ClosureId(i as u32 - 1));
        let mut c = FnCheck::new(p, body, f, closure, &holders, &captured);
        c.asked = asked;
        let (diags, made, probed) = c.run();
        for (key, t) in made {
            let ts = captured.entry(key).or_default();
            if !ts.iter().any(|x| x.place == t.place) {
                ts.push(t);
            }
        }
        each(diags, probed);
    }
}

/// What the memory checker knows before a line runs, for tools (`wrela query`): the loans live
/// there and the places moved out of.
#[derive(Clone, Debug, Default)]
pub struct StateView {
    pub loans: Vec<LoanView>,
    pub moved: Vec<MovedView>,
}

#[derive(Clone, Debug)]
pub struct LoanView {
    /// The place lent, as diagnostics name it (`w.pos`).
    pub place: String,
    pub mutable: bool,
    /// What holds the loan: `` `x` ``, `the result of `f``, `the `match``.
    pub holder: String,
    /// Where the loan is made.
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct MovedView {
    pub place: String,
    /// Moved on some paths here, not all.
    pub maybe: bool,
    /// Where it's moved.
    pub span: Span,
}

/// [`StateView`] before the first statement of `body` (its function's or a closure's) to run
/// that starts in `file` between offsets `lo` and `hi` (a line): `None` if none does.
pub fn state_at(
    p: &Program,
    body: &Body,
    file: wrela_diag::FileId,
    lo: u32,
    hi: u32,
) -> Option<StateView> {
    let mut best: Option<(u32, StateView)> = None;
    run_bodies(p, body, Some((file, lo, hi)), &mut |_, probed| {
        if let Some((start, view)) = probed
            && best.as_ref().is_none_or(|(b, _)| start < *b)
        {
            best = Some((start, view));
        }
    });
    best.map(|(_, v)| v)
}

/// For a closure and a projection it captures: what the projection aliases where the closure
/// is made. The loans that say so are the enclosing body's, so a closure's body would otherwise
/// see accesses through a captured projection reach nothing.
type Captured = HashMap<(ClosureId, Local), Vec<Target>>;

/// A target of a projection a new closure captures, for [`Captured`].
type CapturedTarget = ((ClosureId, Local), Target);

// ---- sets ----------------------------------------------------------------------------------------

/// A set of indices, sorted. The loans and locals that matter at one point are few, so the sets
/// stay small however long the function is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Set(Vec<u32>);

impl Set {
    fn insert(&mut self, i: usize) {
        if let Err(at) = self.0.binary_search(&(i as u32)) {
            self.0.insert(at, i as u32);
        }
    }
    fn remove(&mut self, i: usize) {
        if let Ok(at) = self.0.binary_search(&(i as u32)) {
            self.0.remove(at);
        }
    }
    fn contains(&self, i: usize) -> bool {
        self.0.binary_search(&(i as u32)).is_ok()
    }
    /// `self |= other`; whether anything changed.
    fn union(&mut self, other: &Set) -> bool {
        if other.0.iter().all(|i| self.0.binary_search(i).is_ok()) {
            return false;
        }
        self.0.extend_from_slice(&other.0);
        self.0.sort_unstable();
        self.0.dedup();
        true
    }
    fn retain(&mut self, mut f: impl FnMut(usize) -> bool) {
        self.0.retain(|&i| f(i as usize));
    }
    fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().map(|&i| i as usize)
    }
}

// ---- what statements use ---------------------------------------------------------------------

fn place_uses(p: &Place, out: &mut Vec<Local>) {
    out.push(p.local);
    for proj in &p.proj {
        if let Proj::Index(l) = proj {
            out.push(*l);
        }
    }
}

fn operand_uses(o: &Operand, out: &mut Vec<Local>) {
    if let Some(p) = o.place() {
        place_uses(p, out);
    }
}

fn rvalue_uses(body: &Body, r: &Rvalue, out: &mut Vec<Local>) {
    match r {
        Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::ArrayRepeat(o, _) | Rvalue::Convert(o) => {
            operand_uses(o, out)
        }
        Rvalue::Binary(_, a, b) => {
            operand_uses(a, out);
            operand_uses(b, out);
        }
        Rvalue::Adt { fields: xs, .. }
        | Rvalue::Tuple(xs)
        | Rvalue::Array(xs)
        | Rvalue::Construct(xs) => xs.iter().for_each(|x| operand_uses(x, out)),
        Rvalue::Discriminant(p) | Rvalue::Len(p) => place_uses(p, out),
        Rvalue::Call(c) => {
            if let Callee::Local(l) = c.callee {
                out.push(l);
            }
            for a in &c.args {
                match a {
                    Arg::Borrow(p, _) | Arg::Mut(p, _) => place_uses(p, out),
                    Arg::Take(o) => operand_uses(o, out),
                }
            }
        }
        // A new closure borrows its captures: through what they hold, if they're projections.
        Rvalue::Closure(id) => {
            out.extend(body.closures[id.0 as usize].captures.iter().map(|c| c.0))
        }
        Rvalue::FnRef(..)
        | Rvalue::Const(_)
        | Rvalue::Text(_)
        | Rvalue::Embed(_)
        | Rvalue::ConstParam(_) => {}
        Rvalue::BorrowStruct { fields, .. } => {
            for a in fields {
                match a {
                    Arg::Borrow(p, _) | Arg::Mut(p, _) => place_uses(p, out),
                    Arg::Take(o) => operand_uses(o, out),
                }
            }
        }
        Rvalue::Dispatch(d) => {
            d.groups.iter().for_each(|g| operand_uses(g, out));
            d.args.iter().for_each(|(_, p, _)| place_uses(p, out));
            d.indirect.iter().for_each(|(p, _)| place_uses(p, out));
        }
        Rvalue::Draw(d) => {
            operand_uses(&d.vertices, out);
            operand_uses(&d.instances, out);
            d.args.iter().for_each(|(_, _, p, _)| place_uses(p, out));
            d.indirect.iter().for_each(|(p, _)| place_uses(p, out));
        }
    }
}

/// Adds a statement's uses to `uses`; returns the local it defines (whose old value it's done
/// with).
fn stmt_uses_defs(p: &Program, body: &Body, s: &Statement, uses: &mut Vec<Local>) -> Option<Local> {
    match &s.kind {
        StatementKind::Assign(place, r) => {
            rvalue_uses(body, r, uses);
            if place.proj.is_empty() && !is_bound_alias(p, body, place.local, r) {
                // A whole local gets a new value (or a projection local, a call's place).
                Some(place.local)
            } else {
                // A write through a place: a use of its local.
                place_uses(place, uses);
                None
            }
        }
        StatementKind::Eval(r) => {
            rvalue_uses(body, r, uses);
            None
        }
        StatementKind::Bind { local, place, .. } => {
            place_uses(place, uses);
            Some(*local)
        }
        StatementKind::Check(place) => {
            place_uses(place, uses);
            None
        }
        StatementKind::Live(l) | StatementKind::Dead(l) => Some(*l),
        // A drop at the end of a scope: not a use, as the local's own end isn't.
        StatementKind::Drop(_) => None,
    }
}

/// Whether assigning `r` to the whole local `l` writes through it: `l` aliases a place, and `r`
/// isn't the call that gives it one. A closure or function bound to a name is held, never an
/// alias, whatever the binding's kind.
fn is_bound_alias(p: &Program, body: &Body, l: Local, r: &Rvalue) -> bool {
    let d = body.local(l);
    d.kind.is_projection()
        && !is_callable(&p.types, d.ty)
        && !matches!(r, Rvalue::Call(c) if c.ret_mode != RetMode::Owned)
}

fn term_uses(t: &Terminator, out: &mut Vec<Local>) {
    match &t.kind {
        TerminatorKind::If { cond, .. } => operand_uses(cond, out),
        TerminatorKind::Return(Some(o)) => operand_uses(o, out),
        TerminatorKind::ReturnPlace(p) => place_uses(p, out),
        _ => {}
    }
}

// ---- loans and moves -----------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Loan {
    place: Place,
    mutable: bool,
    /// A call's `mut` argument before the call: shared until the call begins its `mut`
    /// access, so the arguments after it may read the place (§6.5).
    reserved: bool,
    holder: Local,
    /// The projections the loan was reached through, `holder` first: accesses through any of
    /// them are the loan's own.
    via: Vec<Local>,
    /// Whether `place` is exactly what the holder aliases (so a path through the holder
    /// extends it), or only somewhere it points into (a call's result).
    exact: bool,
    /// For a borrow struct holder: the fields (a path, for a borrow struct inside one) whose
    /// projection this loan is. Empty otherwise.
    field: Vec<u32>,
    span: Span,
}

/// A place an access through a projection reaches (see [`FnCheck::resolve`]).
#[derive(Clone, Debug)]
struct Target {
    place: Place,
    via: Vec<Local>,
    exact: bool,
    mutable: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Moved {
    place: Place,
    span: Span,
    /// Moved on some paths here, not all.
    maybe: bool,
    /// Reached here only around a loop's back edge.
    back: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct State {
    /// The loans whose holders are live (a loan ends with its holder's last use).
    loans: Set,
    moves: Vec<Moved>,
    /// Locals in scope that haven't been given their first value yet.
    uninit: Set,
}

impl State {
    fn join(&mut self, other: &State) -> bool {
        let mut changed = self.loans.union(&other.loans);
        changed |= self.uninit.union(&other.uninit);
        // Each side's first move of each place.
        let mut theirs: HashMap<&Place, &Moved> = HashMap::new();
        for n in &other.moves {
            theirs.entry(&n.place).or_insert(n);
        }
        let ours: HashSet<&Place> = self.moves.iter().map(|m| &m.place).collect();
        let mut out: Vec<Moved> = Vec::new();
        for m in &self.moves {
            match theirs.get(&m.place) {
                Some(n) => out.push(Moved {
                    maybe: m.maybe || n.maybe,
                    back: m.back && n.back,
                    ..m.clone()
                }),
                None => out.push(Moved { maybe: true, ..m.clone() }),
            }
        }
        for n in &other.moves {
            if !ours.contains(&n.place) {
                out.push(Moved { maybe: true, ..n.clone() });
            }
        }
        drop(ours);
        if out != self.moves {
            changed = true;
            self.moves = out;
        }
        changed
    }
}

fn overlaps(a: &Place, b: &Place) -> bool {
    if a.local != b.local {
        return false;
    }
    for (x, y) in a.proj.iter().zip(&b.proj) {
        let same = match (x, y) {
            (Proj::Field(i), Proj::Field(j)) => i == j,
            (Proj::Downcast(_), Proj::Downcast(_)) => true,
            (Proj::Comp(i), Proj::Comp(j)) => i == j,
            (Proj::Comp(c), Proj::Swizzle(cs)) | (Proj::Swizzle(cs), Proj::Comp(c)) => {
                cs.contains(c)
            }
            (Proj::Swizzle(a), Proj::Swizzle(b)) => a.iter().any(|c| b.contains(c)),
            // Every element overlaps every other; an element overlaps its vector's components.
            (Proj::Index(_), _) | (_, Proj::Index(_)) => true,
            _ => false,
        };
        if !same {
            return false;
        }
    }
    true
}

/// `p` extended by the projections `more`, a component of a swizzle being the component of the
/// vector it picks (`v.xy` then `.x` is `v.x`), so places compare as the parts they are.
fn extend_path(p: &mut Place, more: &[Proj]) {
    for proj in more {
        let picked = match (p.proj.last(), proj) {
            (Some(Proj::Swizzle(cs)), Proj::Comp(c)) => cs.get(*c as usize).map(|&c| Proj::Comp(c)),
            (Some(Proj::Swizzle(cs)), Proj::Swizzle(ds)) => ds
                .iter()
                .map(|&d| cs.get(d as usize).copied())
                .collect::<Option<Vec<u8>>>()
                .map(Proj::Swizzle),
            _ => None,
        };
        match picked {
            Some(q) => {
                p.proj.pop();
                p.proj.push(q);
            }
            None => p.proj.push(proj.clone()),
        }
    }
}

/// Whether `prefix` is `p` or a place `p` is inside of.
fn is_prefix(prefix: &Place, p: &Place) -> bool {
    prefix.local == p.local
        && prefix.proj.len() <= p.proj.len()
        && prefix.proj.iter().zip(&p.proj).all(|(a, b)| match (a, b) {
            (Proj::Index(_), Proj::Index(_)) => true,
            _ => a == b,
        })
}

/// What happens to a place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    Read,
    Write,
    Move,
    Borrow,
    MutBorrow,
}

/// A program point: a block and a statement in it (the terminator is `stmts.len()`).
type Point = (BlockId, usize);

struct FnCheck<'a> {
    p: &'a Program,
    body: &'a Body,
    f: &'a FnBody,
    closure: Option<ClosureId>,
    /// Per local: whether it can hold loans (a projection, or a closure). Liveness is only
    /// needed for these.
    holders: &'a [bool],
    /// Per block: the holders live on entry. Empty for an unreachable block.
    live_in: Vec<Set>,
    /// Per block: the holders each statement (the terminator is `stmts.len()`) uses or defines
    /// for the last time, by statement. Their loans end there.
    dying: Vec<Vec<(u32, Local)>>,
    loans: Vec<Loan>,
    /// Loans by where they're made and what they're on, so a block revisited makes the same.
    loan_ids: HashMap<(Point, Place, Local, bool), usize>,
    diags: Vec<Diagnostic>,
    emit: bool,
    /// Loans already reported in a conflict: one error per loan, at its first conflict (later
    /// ones are usually its consequences).
    reported: Vec<usize>,
    /// Where conflicts were reported: an argument already reported doesn't get a second error
    /// for overlapping another argument of its call.
    reported_at: Vec<Span>,
    /// What the projections this closure captures alias (see [`Captured`]).
    captured: &'a Captured,
    /// What the projections captured by the closures this body makes alias.
    made: Vec<CapturedTarget>,
    /// For [`state_at`]: the line asked about (its file, and its offsets), and the state before
    /// its first statement (where that statement starts).
    asked: Option<(wrela_diag::FileId, u32, u32)>,
    probed: Option<(u32, State)>,
}

impl<'a> FnCheck<'a> {
    fn new(
        p: &'a Program,
        body: &'a Body,
        f: &'a FnBody,
        closure: Option<ClosureId>,
        holders: &'a [bool],
        captured: &'a Captured,
    ) -> Self {
        FnCheck {
            p,
            body,
            f,
            closure,
            holders,
            live_in: Vec::new(),
            dying: Vec::new(),
            loans: Vec::new(),
            loan_ids: HashMap::new(),
            diags: Vec::new(),
            emit: false,
            reported: Vec::new(),
            reported_at: Vec::new(),
            captured,
            made: Vec::new(),
            asked: None,
            probed: None,
        }
    }

    fn run(mut self) -> (Vec<Diagnostic>, Vec<CapturedTarget>, Option<(u32, StateView)>) {
        let reachable = self.reachable();
        self.liveness(&reachable);
        // Forward: each reachable block's state on entry, to a fixpoint.
        let n = self.f.blocks.len();
        let empty = State { loans: Set::default(), moves: Vec::new(), uninit: Set::default() };
        let mut entry: Vec<Option<State>> = vec![None; n];
        entry[0] = Some(empty);
        // Blocks in reverse postorder, so a block's predecessors (but for back edges) come
        // before it: each block is visited once, but for loops.
        let order = self.reverse_postorder();
        let mut rank = vec![usize::MAX; n];
        for (i, b) in order.iter().enumerate() {
            rank[b.index()] = i;
        }
        let mut work = BTreeSet::from([rank[FnBody::ENTRY.index()]]);
        while let Some(i) = work.pop_first() {
            let b = order[i];
            let Some(st) = entry[b.index()].clone() else { continue };
            let out = self.transfer_block(b, st);
            for s in self.f.successors(b) {
                let mut incoming = out.clone();
                // Holders dead on entry to `s` are redefined there before any use: their loans
                // are over.
                let live = &self.live_in[s.index()];
                incoming.loans.retain(|i| live.contains(self.loans[i].holder.index()));
                if self.f.is_back_edge(b) {
                    for m in &mut incoming.moves {
                        m.back = true;
                    }
                }
                let changed = match &mut entry[s.index()] {
                    Some(e) => e.join(&incoming),
                    slot @ None => {
                        *slot = Some(incoming);
                        true
                    }
                };
                if changed {
                    work.insert(rank[s.index()]);
                }
            }
        }
        // Then once more over each block that has a state (each reachable one), reporting.
        self.emit = true;
        for (i, st) in entry.into_iter().enumerate() {
            if let Some(st) = st {
                self.transfer_block(BlockId(i as u32), st);
            }
        }
        let probed = self.probed.take().map(|(start, st)| (start, self.view(&st)));
        (self.diags, self.made, probed)
    }

    /// A state as [`state_at`] shows it.
    fn view(&self, st: &State) -> StateView {
        let loans = st
            .loans
            .iter()
            .map(|i| {
                let l = &self.loans[i];
                LoanView {
                    place: self.describe(&l.place),
                    mutable: l.mutable && !l.reserved,
                    holder: self.holder_text(l.holder),
                    span: l.span,
                }
            })
            .collect();
        let moved = st
            .moves
            .iter()
            .map(|m| MovedView { place: self.describe(&m.place), maybe: m.maybe, span: m.span })
            .collect();
        StateView { loans, moved }
    }

    /// For [`state_at`]: keeps `st` if the statement at `span` is the first on the line asked
    /// about to run (the blocks are in the order they're written, and so are their statements).
    fn probe(&mut self, st: &State, span: Span) {
        let Some((file, lo, hi)) = self.asked else { return };
        if self.emit && span.file == file && (lo..hi).contains(&span.start) && self.probed.is_none()
        {
            self.probed = Some((span.start, st.clone()));
        }
    }

    /// The reachable blocks, in reverse postorder from the entry.
    fn reverse_postorder(&self) -> Vec<BlockId> {
        let n = self.f.blocks.len();
        let mut seen = vec![false; n];
        let mut post = Vec::new();
        // Each block on the path, with its successors still to visit.
        let mut stack = vec![(FnBody::ENTRY, self.f.successors(FnBody::ENTRY).collect::<Vec<_>>())];
        seen[FnBody::ENTRY.index()] = true;
        while let Some((b, next)) = stack.last_mut() {
            match next.pop() {
                Some(s) if !seen[s.index()] => {
                    seen[s.index()] = true;
                    let succ = self.f.successors(s).collect();
                    stack.push((s, succ));
                }
                Some(_) => {}
                None => {
                    post.push(*b);
                    stack.pop();
                }
            }
        }
        post.reverse();
        post
    }

    fn reachable(&self) -> Vec<bool> {
        let mut seen = vec![false; self.f.blocks.len()];
        let mut work = vec![FnBody::ENTRY];
        while let Some(b) = work.pop() {
            if std::mem::replace(&mut seen[b.index()], true) {
                continue;
            }
            work.extend(self.f.successors(b));
        }
        seen
    }

    fn liveness(&mut self, reachable: &[bool]) {
        let n = self.f.blocks.len();
        // Each block's live holders on entry, to a fixpoint.
        let mut live_in: Vec<Set> = vec![Set::default(); n];
        loop {
            let mut changed = false;
            for b in (0..n).rev() {
                if reachable[b] {
                    let live = self.live_through(BlockId(b as u32), &live_in, |_, _| {});
                    changed |= live_in[b].union(&live);
                }
            }
            if !changed {
                break;
            }
        }
        // Then where each holder is used for the last time, from the fixpoint.
        self.dying = (0..n)
            .map(|b| {
                let mut dying = Vec::new();
                if reachable[b] {
                    self.live_through(BlockId(b as u32), &live_in, |i, l| {
                        dying.push((i as u32, l))
                    });
                    dying.reverse();
                }
                dying
            })
            .collect();
        self.live_in = live_in;
    }

    /// Walks block `b` backward from the holders live on its exit (live on entry to its
    /// successors, by `live_in`), giving `dies` each holder a statement (the terminator is
    /// `stmts.len()`) uses or defines that isn't live after it. Returns the holders live on
    /// entry to `b`.
    fn live_through(&self, b: BlockId, live_in: &[Set], mut dies: impl FnMut(usize, Local)) -> Set {
        let mut live = Set::default();
        for s in self.f.successors(b) {
            live.union(&live_in[s.index()]);
        }
        let block = self.f.block(b);
        let mut at = |i: usize, uses: &[Local], def: Option<Local>, live: &mut Set| {
            for l in uses.iter().copied().chain(def) {
                if self.holders[l.index()] && !live.contains(l.index()) {
                    dies(i, l);
                    // Once per statement.
                    live.insert(l.index());
                }
            }
            if let Some(d) = def {
                live.remove(d.index());
            }
            for u in uses {
                if self.holders[u.index()] {
                    live.insert(u.index());
                }
            }
        };
        let mut uses = Vec::new();
        term_uses(&block.term, &mut uses);
        at(block.stmts.len(), &uses, None, &mut live);
        for (i, s) in block.stmts.iter().enumerate().rev() {
            uses.clear();
            let def = stmt_uses_defs(self.p, self.body, s, &mut uses);
            at(i, &uses, def, &mut live);
        }
        live
    }

    /// The holders point `at` uses or defines for the last time.
    fn dying_at(&self, at: Point) -> impl Iterator<Item = Local> + '_ {
        let d = &self.dying[at.0.index()];
        let i = at.1 as u32;
        d[d.partition_point(|&(j, _)| j < i)..]
            .iter()
            .take_while(move |&&(j, _)| j == i)
            .map(|&(_, l)| l)
    }

    /// Whether `l` is used after point `at`: a loan it holds is live there.
    fn live_after(&self, at: Point, l: Local) -> bool {
        !self.dying_at(at).any(|d| d == l)
    }

    fn local(&self, l: Local) -> &'a LocalDecl {
        self.body.local(l)
    }

    fn is_alias(&self, l: Local) -> bool {
        self.local(l).kind.is_projection()
    }

    /// For a place through a borrow struct's projection field: that field's mode, from the
    /// last such field it crosses (a run field is `borrow`). `None` for a place that crosses
    /// none.
    fn borrow_field_mode(&self, place: &Place) -> Option<RetMode> {
        let mut t = self.local(place.local).ty;
        let mut variant = None;
        let mut out = None;
        for proj in &place.proj {
            if let (Proj::Field(i), crate::ty::TyKind::Adt(a, _)) = (proj, self.p.types.kind(t))
                && self.p.adt(*a).borrow
                && let Some(f) = self.p.adt_fields(*a, None).get(*i as usize)
            {
                let run = matches!(
                    self.p.types.kind(f.ty),
                    crate::ty::TyKind::Slice(_) | crate::ty::TyKind::Str
                );
                if f.mode != RetMode::Owned || run {
                    out = Some(if run { RetMode::Borrow } else { f.mode });
                }
            }
            t = crate::mir::proj_ty_pub(self.p, t, &mut variant, proj);
        }
        out
    }

    /// Whether `l` is outside the closure being checked (a capture).
    fn is_capture(&self, l: Local) -> bool {
        is_capture(&self.body.locals, self.closure, l)
    }

    fn describe(&self, p: &Place) -> String {
        describe(self.p, &self.body.locals, p)
    }

    fn err(&mut self, d: Diagnostic) {
        if self.emit {
            self.diags.push(d);
        }
    }

    // ---- transfer ------------------------------------------------------------------------------

    fn transfer_block(&mut self, b: BlockId, mut st: State) -> State {
        let block = self.f.block(b);
        for (i, s) in block.stmts.iter().enumerate() {
            self.probe(&st, s.span);
            self.statement(&mut st, (b, i), s);
            self.end_loans(&mut st, (b, i));
        }
        let at = (b, block.stmts.len());
        self.probe(&st, block.term.span);
        match &block.term.kind {
            TerminatorKind::If { cond, .. } => self.operand(&mut st, at, cond),
            TerminatorKind::Return(Some(o)) => {
                self.operand(&mut st, at, o);
                self.returned_borrow_struct(&st, o);
            }
            TerminatorKind::ReturnPlace(place) => self.return_place(&mut st, at, place),
            _ => {}
        }
        self.end_loans(&mut st, at);
        st
    }

    /// The loans of the holders `at` uses for the last time end after it.
    fn end_loans(&self, st: &mut State, at: Point) {
        if self.dying_at(at).next().is_some() {
            st.loans.retain(|i| self.live_after(at, self.loans[i].holder));
        }
    }

    fn statement(&mut self, st: &mut State, at: Point, s: &Statement) {
        match &s.kind {
            StatementKind::Assign(place, r) => {
                self.rvalue(st, at, r, s.span);
                if place.proj.is_empty() && !is_bound_alias(self.p, self.body, place.local, r) {
                    // The whole local gets a new value: its first (a binding's), or a new one,
                    // which is a write.
                    let l = place.local;
                    let first = st.uninit.contains(l.index())
                        || !matches!(self.local(l).kind, LocalKind::User(_));
                    if !first {
                        self.write(st, at, place, s.span);
                    }
                    self.redefine(st, l);
                    st.uninit.remove(l.index());
                    match r {
                        Rvalue::Call(c) if c.ret_mode != RetMode::Owned => {
                            self.call_result_loans(st, at, place.local, c, s.span)
                        }
                        Rvalue::Call(c) if self.p.is_borrow_struct(self.local(place.local).ty) => {
                            self.borrow_struct_result_loans(st, at, place.local, c, s.span)
                        }
                        Rvalue::Closure(id) => self.closure_loans(st, at, place.local, *id, s.span),
                        Rvalue::BorrowStruct { fields, .. } => {
                            self.borrow_struct_loans(st, at, place.local, fields, s.span)
                        }
                        Rvalue::Use(Operand {
                            kind: OperandKind::Copy(src) | OperandKind::Move(src, _),
                            ty,
                            ..
                        }) if src.proj.is_empty()
                            && (is_callable(&self.p.types, *ty)
                                || self.p.is_borrow_struct(*ty)) =>
                        {
                            // A closure held by another local, copied or moved (`take f`): its
                            // loans travel with it.
                            self.inherit(st, at, place.local, src.local, s.span)
                        }
                        _ => {}
                    }
                } else {
                    self.write(st, at, place, s.span);
                }
            }
            StatementKind::Eval(r) => self.rvalue(st, at, r, s.span),
            StatementKind::Check(p) => self.access(st, at, p, Access::Read, s.span),
            StatementKind::Bind { local, place, mutable } => {
                self.bind(st, at, *local, place, *mutable, s.span)
            }
            StatementKind::Live(l) => {
                self.redefine(st, *l);
                st.uninit.insert(l.index());
            }
            StatementKind::Dead(l) => {
                self.out_of_scope(st, at, *l, s.span);
                self.redefine(st, *l);
                st.uninit.remove(l.index());
            }
            // The owner is done with the value (its scope ends, or it's overwritten next): what
            // still borrows it is caught where the scope ends (`Dead`) or by the write.
            StatementKind::Drop(_) => {}
        }
    }

    /// `l`'s scope ends (`span` is its block): nothing still live may borrow it, as a closure
    /// made in the block and kept after it does (§6.7).
    fn out_of_scope(&mut self, st: &State, at: Point, l: Local, span: Span) {
        if !self.emit {
            return;
        }
        let kept = st.loans.iter().find(|&i| {
            let loan = &self.loans[i];
            loan.place.local == l
                && loan.holder != l
                && self.live_after(at, loan.holder)
                && !self.returned(loan.holder)
        });
        if let Some(i) = kept {
            let borrowed = self.loans[i].span;
            let holder_ty = self.local(self.loans[i].holder).ty;
            let what = self.describe(&Place::local(l));
            let end = Span::new(span.file, span.end.saturating_sub(1), span.end);
            let note = if self.p.is_borrow_struct(holder_ty) {
                "a borrow struct borrows its fields' places for as long as it's kept (§6.6)"
            } else {
                "a closure borrows what it uses for as long as it's kept (§6.7)"
            };
            self.err(
                Diagnostic::new(
                    codes::E0510,
                    borrowed,
                    format!("`{what}` goes out of scope while this still borrows it"),
                )
                .with_secondary(end, format!("`{what}`'s scope ends here"))
                .with_note(note)
                .with_help(format!("declare `{what}` outside the block, so it lives as long")),
            );
        }
    }

    /// Whether the code returns local `l`'s value (a borrow struct it returns is checked at the
    /// return instead, E0508).
    fn returned(&self, l: Local) -> bool {
        self.f.blocks.iter().any(|b| match &b.term.kind {
            TerminatorKind::Return(Some(Operand {
                kind: OperandKind::Copy(p) | OperandKind::Move(p, _),
                ..
            })) => p.local == l && self.p.is_borrow_struct(self.local(l).ty),
            _ => false,
        })
    }

    /// `l` gets a new value (or goes out of scope): its loans end, and what was moved out of
    /// it is gone with the old value.
    fn redefine(&mut self, st: &mut State, l: Local) {
        st.loans.retain(|i| self.loans[i].holder != l);
        st.moves.retain(|m| m.place.local != l);
    }

    fn new_loan(&mut self, st: &mut State, at: Point, loan: Loan) {
        debug_assert!(self.holders[loan.holder.index()], "liveness tracks every loan's holder");
        let key = (at, loan.place.clone(), loan.holder, loan.mutable);
        let id = match self.loan_ids.get(&key) {
            Some(&i) => {
                self.loans[i] = loan;
                i
            }
            None => {
                self.loans.push(loan);
                let i = self.loans.len() - 1;
                self.loan_ids.insert(key, i);
                i
            }
        };
        st.loans.insert(id);
    }

    fn operand(&mut self, st: &mut State, at: Point, o: &Operand) {
        match &o.kind {
            OperandKind::Copy(p) | OperandKind::Move(p, MoveKind::Unmarked) => {
                self.access(st, at, p, Access::Read, o.span)
            }
            OperandKind::Move(p, _) => self.access(st, at, p, Access::Move, o.span),
            OperandKind::Const(_) => {}
        }
    }

    fn rvalue(&mut self, st: &mut State, at: Point, r: &Rvalue, span: Span) {
        match r {
            Rvalue::Use(o)
            | Rvalue::Unary(_, o)
            | Rvalue::ArrayRepeat(o, _)
            | Rvalue::Convert(o) => self.operand(st, at, o),
            Rvalue::Binary(_, a, b) => {
                self.operand(st, at, a);
                self.operand(st, at, b);
            }
            Rvalue::Adt { fields: xs, .. }
            | Rvalue::Tuple(xs)
            | Rvalue::Array(xs)
            | Rvalue::Construct(xs) => xs.iter().for_each(|x| self.operand(st, at, x)),
            Rvalue::Discriminant(p) | Rvalue::Len(p) => self.access(st, at, p, Access::Read, span),
            Rvalue::Call(c) => {
                for a in &c.args {
                    match a {
                        // Bound (and checked) where the argument was evaluated.
                        Arg::Borrow(..) => {}
                        Arg::Mut(p, span) => {
                            if !self.is_alias(p.local) {
                                self.access(st, at, p, Access::MutBorrow, *span);
                            } else if self.local(p.local).kind == (LocalKind::Arg { mutable: true })
                            {
                                let args: Vec<Local> = c
                                    .args
                                    .iter()
                                    .filter_map(|a| a.place().map(|p| p.local))
                                    .collect();
                                self.activate(st, at, p.local, &args, *span);
                            }
                        }
                        Arg::Take(o) => self.operand(st, at, o),
                    }
                }
            }
            Rvalue::Closure(_)
            | Rvalue::FnRef(..)
            | Rvalue::Const(_)
            | Rvalue::Text(_)
            | Rvalue::Embed(_)
            | Rvalue::ConstParam(_) => {}
            Rvalue::BorrowStruct { fields, .. } => {
                for a in fields {
                    match a {
                        Arg::Borrow(p, span) => self.access(st, at, p, Access::Borrow, *span),
                        Arg::Mut(p, span) => self.access(st, at, p, Access::MutBorrow, *span),
                        Arg::Take(o) => self.operand(st, at, o),
                    }
                }
            }
            Rvalue::Dispatch(d) => d.groups.iter().for_each(|g| self.operand(st, at, g)),
            Rvalue::Draw(d) => {
                self.operand(st, at, &d.vertices);
                self.operand(st, at, &d.instances);
            }
        }
    }

    /// The places an access through `place` touches: each with the projections it's through
    /// (empty for a place of a local that holds its value), whether it's exactly what the
    /// projection aliases, and whether it may be written through it.
    fn resolve(&self, st: &State, place: &Place) -> Vec<Target> {
        let own = || Target { place: place.clone(), via: Vec::new(), exact: true, mutable: true };
        // Through a borrow struct's projection field: what the field projects, from the loans
        // under it; with none (a parameter's), the place itself.
        if !self.is_alias(place.local) && self.p.is_borrow_struct(self.local(place.local).ty) {
            if self.borrow_field_mode(place).is_none() && !place.proj.is_empty() {
                return vec![own()];
            }
            let path: Vec<Option<u32>> = place
                .proj
                .iter()
                .map(|x| match x {
                    Proj::Field(i) => Some(*i),
                    _ => None,
                })
                .collect();
            let mut out: Vec<Target> = Vec::new();
            for i in st.loans.iter() {
                let l = &self.loans[i];
                if l.holder != place.local || l.field.is_empty() {
                    continue;
                }
                // The loan's field path and the place's agree as far as both go: the rest of a
                // longer place extends the loan's place; a shorter one covers the loan whole.
                let n = l.field.len().min(path.len());
                if l.field[..n].iter().zip(&path[..n]).any(|(&f, &p)| Some(f) != p) {
                    continue;
                }
                let mut target = l.place.clone();
                if path.len() > l.field.len() && l.exact {
                    extend_path(&mut target, &place.proj[l.field.len()..]);
                }
                out.push(Target {
                    place: target,
                    via: l.via.clone(),
                    exact: l.exact,
                    mutable: l.mutable,
                });
            }
            if out.is_empty() {
                return vec![own()];
            }
            return out;
        }
        if !self.is_alias(place.local) {
            return vec![own()];
        }
        // A projection this closure captures: what it aliased where the closure was made (or,
        // not known, itself, so accesses through it still meet each other).
        if self.is_capture(place.local)
            && let Some(c) = self.closure
        {
            let Some(ts) = self.captured.get(&(c, place.local)) else { return vec![own()] };
            return ts
                .iter()
                .map(|t| {
                    let mut t = t.clone();
                    if t.exact {
                        extend_path(&mut t.place, &place.proj);
                    }
                    t
                })
                .collect();
        }
        let mut out: Vec<Target> = Vec::new();
        for i in st.loans.iter() {
            let l = &self.loans[i];
            if l.holder != place.local {
                continue;
            }
            let target = if l.exact {
                let mut t = l.place.clone();
                extend_path(&mut t, &place.proj);
                t
            } else {
                l.place.clone()
            };
            match out.iter_mut().find(|t| t.place == target) {
                Some(t) => t.mutable |= l.mutable,
                None => out.push(Target {
                    place: target,
                    via: l.via.clone(),
                    exact: l.exact,
                    mutable: l.mutable,
                }),
            }
        }
        out
    }

    /// `local` starts aliasing `place`.
    fn bind(
        &mut self,
        st: &mut State,
        at: Point,
        local: Local,
        place: &Place,
        mutable: bool,
        span: Span,
    ) {
        let kind = if mutable { Access::MutBorrow } else { Access::Borrow };
        let targets = self.resolve(st, place);
        // What `local` held before (a loop's last pass) ends here, before the new loan's
        // access is checked against the others.
        self.redefine(st, local);
        self.access(st, at, place, kind, span);
        // A call's `mut` argument is only reserved until the call (§6.5).
        let reserve = mutable && self.local(local).kind == LocalKind::Arg { mutable: true };
        for t in targets {
            if reserve {
                let reserved = t.mutable;
                let mut via = vec![local];
                via.extend(t.via);
                let loan = Loan {
                    place: t.place,
                    mutable: false,
                    reserved,
                    holder: local,
                    via,
                    exact: t.exact,
                    field: Vec::new(),
                    span,
                };
                self.new_loan(st, at, loan);
            } else {
                self.lend(st, at, local, t, mutable, span);
            }
        }
    }

    /// A call begins the `mut` access its argument `holder` reserved: it now conflicts with
    /// every other live loan on the place, and holds a `mut` loan on it.
    /// The call's other arguments (`args`) are used by the call itself, so their loans count
    /// even though their holders die there.
    fn activate(&mut self, st: &mut State, at: Point, holder: Local, args: &[Local], span: Span) {
        let reserved: Vec<Loan> = st
            .loans
            .iter()
            .map(|i| &self.loans[i])
            .filter(|l| l.holder == holder && l.reserved)
            .cloned()
            .collect();
        for l in reserved {
            // Two `mut` arguments that overlap were reported where the second was evaluated.
            let conflict = st.loans.iter().find(|&i| {
                let o = &self.loans[i];
                let other_mut_arg = args.contains(&o.holder)
                    && self.local(o.holder).kind == (LocalKind::Arg { mutable: true });
                !l.via.contains(&o.holder)
                    && !other_mut_arg
                    && (args.contains(&o.holder) || self.live_after(at, o.holder))
                    && overlaps(&o.place, &l.place)
            });
            if let Some(i) = conflict
                && !(self.emit
                    && (self.reported.contains(&i)
                        || self.reported_at.contains(&self.loans[i].span)))
            {
                if self.emit {
                    self.reported.push(i);
                }
                self.conflict(&l.place, i, Access::MutBorrow, span);
            }
            self.new_loan(st, at, Loan { mutable: true, reserved: false, ..l });
        }
    }

    /// `holder` gets a loan on target `t`, through `t`'s projections. A `mut` loan through a
    /// projection writes only what that one may write.
    fn lend(
        &mut self,
        st: &mut State,
        at: Point,
        holder: Local,
        t: Target,
        mutable: bool,
        span: Span,
    ) {
        let mut via = vec![holder];
        via.extend(t.via);
        let mutable = mutable && t.mutable;
        let reserved = false;
        let field = Vec::new();
        self.new_loan(
            st,
            at,
            Loan { place: t.place, mutable, reserved, holder, via, exact: t.exact, field, span },
        );
    }

    /// A borrow struct's loans (§6.6): each projection field's on its place, in the field's
    /// mode, and a borrow struct field's own loans, under that field.
    fn borrow_struct_loans(
        &mut self,
        st: &mut State,
        at: Point,
        holder: Local,
        fields: &[Arg],
        span: Span,
    ) {
        for (i, a) in fields.iter().enumerate() {
            let (p, mutable) = match a {
                Arg::Borrow(p, _) => (p, false),
                Arg::Mut(p, _) => (p, true),
                Arg::Take(_) => continue,
            };
            let ty = place_ty(self.p, &self.body.locals, p);
            if self.p.is_borrow_struct(ty) {
                // A borrow struct inside this one: its loans, under this field.
                let inner: Vec<Loan> = self
                    .resolve_loans(st, p)
                    .into_iter()
                    .map(|(l, rest)| {
                        let mut field = vec![i as u32];
                        field.extend(rest);
                        Loan { holder, via: vec![holder], field, span, ..l }
                    })
                    .collect();
                for l in inner {
                    self.new_loan(st, at, l);
                }
                continue;
            }
            for t in self.resolve(st, p) {
                let mut via = vec![holder];
                via.extend(t.via);
                let loan = Loan {
                    place: t.place,
                    mutable: mutable && t.mutable,
                    reserved: false,
                    holder,
                    via,
                    exact: t.exact,
                    field: vec![i as u32],
                    span,
                };
                self.new_loan(st, at, loan);
            }
        }
    }

    /// The loans of the borrow struct at `p` (a holder, or a borrow struct field of one), each
    /// with the rest of its field path below `p`.
    fn resolve_loans(&self, st: &State, p: &Place) -> Vec<(Loan, Vec<u32>)> {
        let fields: Option<Vec<u32>> = p
            .proj
            .iter()
            .map(|x| match x {
                Proj::Field(i) => Some(*i),
                _ => None,
            })
            .collect();
        let Some(fields) = fields else { return Vec::new() };
        st.loans
            .iter()
            .map(|i| &self.loans[i])
            .filter(|l| l.holder == p.local && l.field.starts_with(&fields))
            .map(|l| (l.clone(), l.field[fields.len()..].to_vec()))
            .collect()
    }

    /// What a projection-returning call returned: somewhere inside its `borrow` and `mut`
    /// arguments (writable through only if it returned `mut`, from a `mut` argument).
    fn call_result_loans(
        &mut self,
        st: &mut State,
        at: Point,
        holder: Local,
        c: &Call,
        span: Span,
    ) {
        // The callee's parameter types and result type, to see which arguments the result can
        // point into.
        let sig = match &c.callee {
            Callee::Fn { func, .. } | Callee::TraitMethod { method: func, .. } => {
                let def = self.p.func(*func);
                Some((def.params.iter().map(|ps| ps.ty).collect::<Vec<_>>(), def.ret))
            }
            _ => None,
        };
        for (k, a) in c.args.iter().enumerate() {
            if let Some((params, ret)) = &sig
                && let Some(&pt) = params.get(k)
                && !crate::traits::can_hold(self.p, pt, *ret)
            {
                continue;
            }
            let (p, from_mut) = match a {
                // A `mut` projection can't come from a read-only place, so a `-> mut T` result
                // borrows only the `mut` arguments (§6.4).
                Arg::Borrow(..) if c.ret_mode == RetMode::Mut => continue,
                Arg::Borrow(p, _) => (p, false),
                Arg::Mut(p, _) => (p, true),
                Arg::Take(_) => continue,
            };
            let mutable = from_mut && c.ret_mode == RetMode::Mut;
            for t in self.resolve(st, p) {
                self.lend(st, at, holder, Target { exact: false, ..t }, mutable, span);
            }
        }
    }

    /// What a call that returns a borrow struct returned (§6.6): each field borrows what the
    /// call's arguments could give it, as a projection-returning call's result does (§6.4). A
    /// `mut` field borrows the `mut` arguments, mutably; any other projection or run field
    /// borrows every `borrow` and `mut` argument. Only arguments whose type can hold the field's
    /// are borrowed.
    fn borrow_struct_result_loans(
        &mut self,
        st: &mut State,
        at: Point,
        holder: Local,
        c: &Call,
        span: Span,
    ) {
        let ty = self.local(holder).ty;
        let crate::ty::TyKind::Adt(a, args) = self.p.types.kind(ty) else { return };
        let (a, args) = (*a, args.clone());
        let params: Option<Vec<crate::ty::TyId>> = match &c.callee {
            Callee::Fn { func, .. } | Callee::TraitMethod { method: func, .. } => {
                Some(self.p.func(*func).params.iter().map(|ps| ps.ty).collect())
            }
            _ => None,
        };
        let modes: Vec<RetMode> = self.p.adt_fields(a, None).iter().map(|f| f.mode).collect();
        let tys = self.p.fields_of(a, &args, None);
        for (i, (mode, fty)) in modes.into_iter().zip(tys).enumerate() {
            let run = matches!(
                self.p.types.kind(fty),
                crate::ty::TyKind::Slice(_) | crate::ty::TyKind::Str
            );
            let nested = self.p.is_borrow_struct(fty);
            if mode == RetMode::Owned && !run && !nested {
                continue; // a `Copy` value
            }
            let mutable = mode == RetMode::Mut || (nested && has_mut_field(self.p, fty));
            for (k, arg) in c.args.iter().enumerate() {
                if let Some(pt) = params.as_ref().and_then(|ps| ps.get(k))
                    && !crate::traits::can_hold(self.p, *pt, fty)
                {
                    continue;
                }
                let p = match arg {
                    Arg::Borrow(_, _) if mutable => continue,
                    Arg::Borrow(p, _) | Arg::Mut(p, _) => p,
                    Arg::Take(_) => continue,
                };
                let from_mut = matches!(arg, Arg::Mut(..));
                for t in self.resolve(st, p) {
                    let mut via = vec![holder];
                    via.extend(t.via);
                    let loan = Loan {
                        place: t.place,
                        mutable: mutable && from_mut && t.mutable,
                        reserved: false,
                        holder,
                        via,
                        exact: false,
                        field: vec![i as u32],
                        span,
                    };
                    self.new_loan(st, at, loan);
                }
            }
        }
    }

    /// E0508 when a borrow struct a function returns borrows anything but its parameters (or a
    /// constant): what it borrows must outlive the call (§6.4, §6.6).
    fn returned_borrow_struct(&mut self, st: &State, o: &Operand) {
        if !self.emit || !self.p.is_borrow_struct(o.ty) {
            return;
        }
        let (OperandKind::Copy(p) | OperandKind::Move(p, _)) = &o.kind else { return };
        for (loan, _) in self.resolve_loans(st, p) {
            let decl = self.local(loan.place.local);
            // A `mut` loan on a read-only place is E0512 where it's taken.
            let ok = matches!(
                decl.kind,
                LocalKind::User(thir::LocalKind::Param(Mode::Mut | Mode::Borrow))
                    | LocalKind::Const(_)
            );
            if !ok {
                let what = self.describe(&loan.place);
                self.err(
                    Diagnostic::new(
                        codes::E0508,
                        loan.span,
                        format!("a returned borrow struct must borrow the function's parameters, and this borrows `{what}`"),
                    )
                    .with_note("what a borrow struct borrows must outlive the call that returns it; this function's locals end when it returns (§6.6)")
                    .with_help("borrow a `borrow` or `mut` parameter's place instead, or return an owned value"),
                );
                return;
            }
        }
    }

    /// A closure borrows its captures while the local holding it is live: mutably those it
    /// writes.
    fn closure_loans(
        &mut self,
        st: &mut State,
        at: Point,
        holder: Local,
        id: ClosureId,
        span: Span,
    ) {
        let body = self.body;
        let used = captured_places(body, id);
        for &(l, written) in &body.closures[id.0 as usize].captures {
            let whole = Place::local(l);
            // A `Copy` value the closure only reads is copied when it's made (§6.7): read now,
            // and not lent.
            if !written
                && crate::traits::implements_builtin(
                    self.p,
                    self.local(l).ty,
                    crate::defs::Lang::Copy,
                )
            {
                self.access(st, at, &whole, Access::Read, span);
                continue;
            }
            if self.emit && self.is_alias(l) {
                let targets = self.resolve(st, &whole);
                self.made.extend(targets.iter().map(|t| ((id, l), t.clone())));
            }
            // What the body uses of the capture (`v.y`, not `v`), else the whole of it; a place
            // inside another it uses is part of that one.
            let mut places = match used.get(&l) {
                Some(ps) if !ps.is_empty() => ps.clone(),
                _ => vec![(whole, written)],
            };
            let all = places.clone();
            places.retain(|(p, _)| !all.iter().any(|(q, _)| q != p && is_prefix(q, p)));
            for (p, w) in &mut places {
                *w |= all.iter().any(|(q, qw)| *qw && is_prefix(p, q));
            }
            // Each is checked before any is lent, so they don't meet each other.
            let mut lent = Vec::new();
            for (place, w) in places {
                let kind = if w { Access::MutBorrow } else { Access::Borrow };
                lent.push((self.resolve(st, &place), w));
                self.access(st, at, &place, kind, span);
            }
            for (targets, w) in lent {
                for t in targets {
                    self.lend(st, at, holder, t, w, span);
                }
            }
        }
    }

    /// `to` holds what `from` holds (a closure copied to another local).
    fn inherit(&mut self, st: &mut State, at: Point, to: Local, from: Local, span: Span) {
        let held: Vec<Loan> = st
            .loans
            .iter()
            .filter(|&i| self.loans[i].holder == from)
            .map(|i| self.loans[i].clone())
            .collect();
        for l in held {
            let mut via = vec![to];
            via.extend(l.via.iter().copied());
            self.new_loan(st, at, Loan { holder: to, via, span, ..l });
        }
    }

    fn write(&mut self, st: &mut State, at: Point, place: &Place, span: Span) {
        self.access(st, at, place, Access::Write, span);
    }

    // ---- accesses -----------------------------------------------------------------------------

    /// `d` with help to copy `place` (`what`) instead, if its type is `Clone`.
    fn clone_help(&self, d: Diagnostic, place: &Place, what: &str) -> Diagnostic {
        let ty = crate::mir::place_ty(self.p, &self.body.locals, place);
        if crate::traits::implements_builtin(self.p, ty, crate::defs::Lang::Clone) {
            d.with_help(format!("use `{what}.clone()`"))
        } else {
            d
        }
    }

    /// Checks an access to `place`, then applies it (a move, or a reinitialization).
    fn access(&mut self, st: &mut State, at: Point, place: &Place, access: Access, span: Span) {
        let root = place.local;
        let alias = self.is_alias(root);
        // A capture is the enclosing function's place: written only if captured mutably (the
        // checker of the enclosing function saw to that), and never moved. The closure's own
        // projections and arguments still mustn't overlap it.
        if !alias && self.is_capture(root) {
            if access == Access::Move {
                if self.emit {
                    let what = self.describe(place);
                    let d = Diagnostic::new(
                        codes::E0502,
                        span,
                        format!("can't move `{what}` out of the closure's surroundings"),
                    )
                    .with_note("a closure borrows what it uses; it doesn't own it (§6.7)");
                    let d = self.clone_help(d, place, &what);
                    self.err(d);
                }
                return;
            }
            self.check_target(st, at, place, &[], access, span);
            return;
        }
        // Through a borrow struct's projection field, the field's mode decides, not the root's.
        if let Some(mode) = self.borrow_field_mode(place) {
            match access {
                Access::Write | Access::MutBorrow if mode != RetMode::Mut => {
                    self.read_only(place, access, span);
                    return;
                }
                Access::Move => {
                    self.not_owned(place, span);
                    return;
                }
                _ => {}
            }
            for t in self.resolve(st, place) {
                self.check_target(st, at, &t.place, &t.via, access, span);
            }
            return;
        }
        // The root must allow the access (a projection never owns, so never moves).
        let kind = self.local(root).kind;
        match access {
            Access::Write | Access::MutBorrow if !kind.writable() => {
                self.read_only(place, access, span);
                return;
            }
            Access::Move if !kind.owns_value() => {
                self.not_owned(place, span);
                return;
            }
            Access::Move if place.proj.iter().any(|p| matches!(p, Proj::Index(_))) => {
                if self.emit {
                    let what = self.describe(place);
                    let d = Diagnostic::new(
                        codes::E0502,
                        span,
                        format!("can't move out of `{what}`: it's an element of an array"),
                    )
                    .with_note(
                        "an element can't be moved out on its own; the array would have a hole",
                    );
                    let d = self.clone_help(d, place, &what);
                    self.err(d);
                }
                return;
            }
            _ => {}
        }
        // Through a projection, the access is to what it aliases. A write reaches only what
        // the projection may write: a `-> mut T` result points into its `mut` arguments, not
        // its `borrow` ones (E0512).
        if alias {
            for t in self.resolve(st, place) {
                if matches!(access, Access::Write | Access::MutBorrow) && !t.mutable {
                    continue;
                }
                self.check_target(st, at, &t.place, &t.via, access, span);
            }
        } else {
            self.check_target(st, at, place, &[], access, span);
        }
    }

    /// The moves and loans an access to `target` (a place of a local holding its value) meets,
    /// through the projections `via`; then the access's effect.
    fn check_target(
        &mut self,
        st: &mut State,
        at: Point,
        target: &Place,
        via: &[Local],
        access: Access,
        span: Span,
    ) {
        // 1. Moved? A write gives a new value to what was moved out of the place it writes,
        // but not to a part of a value moved whole (`w.a = ..` after `take w`), whichever of
        // the moves comes first.
        let blocking = st.moves.iter().find(|m| {
            overlaps(&m.place, target)
                && !(access == Access::Write && m.place.proj.len() >= target.proj.len())
        });
        if let Some(m) = blocking {
            self.moved(target, m, span);
            return;
        }
        // 2. Loans held by others, live here, on an overlapping place.
        let conflict = st.loans.iter().find(|&i| {
            let l = &self.loans[i];
            !via.contains(&l.holder)
                && self.live_after(at, l.holder)
                && overlaps(&l.place, target)
                && (l.mutable || matches!(access, Access::Write | Access::Move | Access::MutBorrow))
        });
        if let Some(i) = conflict
            && !(self.emit && self.reported.contains(&i))
        {
            if self.emit {
                self.reported.push(i);
            }
            self.conflict(target, i, access, span);
        }
        // 3. Apply.
        let tracked = matches!(self.local(target.local).kind, LocalKind::User(_));
        match access {
            Access::Move if tracked => {
                st.moves.push(Moved { place: target.clone(), span, maybe: false, back: false })
            }
            Access::Write => st.moves.retain(|m| !is_prefix(target, &m.place)),
            _ => {}
        }
    }

    fn moved(&mut self, target: &Place, m: &Moved, span: Span) {
        if !self.emit {
            return;
        }
        let what = self.describe(target);
        let moved = self.describe(&m.place);
        let d = if m.back {
            let mut e = Diagnostic::new(
                codes::E0515,
                span,
                format!("`{moved}` is moved inside this loop, so the next iteration can't use it"),
            );
            if m.span != span {
                e = e.with_secondary(m.span, "moved here");
            }
            e.with_note("a value from outside a loop can be moved inside it at most once")
                .with_help(format!(
                    "move `{moved}` before the loop, or move `{moved}.clone()` inside it"
                ))
        } else if m.maybe {
            Diagnostic::new(codes::E0516, span, format!("`{what}` might have been moved already"))
                .with_secondary(m.span, format!("`{moved}` is moved here on some paths"))
                .with_help("move it on every path, or `.clone()` it where it's moved")
        } else {
            Diagnostic::new(
                codes::E0500,
                span,
                format!("`{what}` was moved, so it can't be used here"),
            )
            .with_secondary(m.span, format!("`{moved}` is moved here"))
            .with_help(format!("pass `{moved}.clone()` where it's moved if you still need it here"))
        };
        self.err(d);
    }

    /// How a loan's holder is named in a diagnostic.
    fn holder_text(&self, h: Local) -> String {
        let d = self.local(h);
        match d.kind {
            LocalKind::TempProjection { role: TempRole::CallResult, .. } => {
                format!("the result of `{}`", d.name)
            }
            _ if self.is_match_holder(h) => "the `match`".into(),
            _ => format!("`{}`", d.name),
        }
    }

    /// Whether `h` is a `match`'s scrutinee, borrowed while the arms test it (mir::build).
    fn is_match_holder(&self, h: Local) -> bool {
        let d = self.local(h);
        d.kind == LocalKind::TempProjection { mutable: false, role: TempRole::Match }
    }

    /// `target` overlaps loan `i`.
    fn conflict(&mut self, target: &Place, i: usize, access: Access, span: Span) {
        if !self.emit {
            return;
        }
        self.reported_at.push(span);
        let l = &self.loans[i];
        let what = self.describe(target);
        let other = self.describe(&l.place);
        let d = if let LocalKind::Arg { .. } = self.local(l.holder).kind {
            Diagnostic::new(
                codes::E0513,
                span,
                match (what == other, l.mutable) {
                    (true, true) => format!("`{what}` is already passed `mut` in this call"),
                    (true, false) => format!("`{what}` is already passed in this call"),
                    (false, true) => {
                        format!(
                            "`{what}` overlaps `{other}`, which this call already takes mutably"
                        )
                    }
                    (false, false) => {
                        format!("`{what}` overlaps `{other}`, which this call already takes")
                    }
                },
            )
            .with_secondary(l.span, format!("`{other}` is passed here"))
            .with_note("in one call, arguments can't overlap when one of them is `mut` (§6.5)")
        } else {
            let hn = self.holder_text(l.holder);
            let msg = if l.mutable {
                format!("`{what}` overlaps `{other}`, which {hn} is borrowing mutably")
            } else {
                format!("`{what}` can't be changed while {hn} borrows `{other}`")
            };
            let code = if l.mutable { codes::E0506 } else { codes::E0507 };
            let live = if self.is_match_holder(l.holder) {
                "a later arm tests it, so the borrow is still live".to_string()
            } else {
                format!("{hn} is used later, so the borrow is still live")
            };
            let mut d = Diagnostic::new(code, span, msg)
                .with_secondary(l.span, format!("{hn} borrows `{other}` here"))
                .with_note(live);
            if l.mutable
                && matches!(access, Access::Borrow | Access::Read)
                && target.proj.len() < l.place.proj.len()
            {
                d = d.with_help(format!("pass only the parts that don't overlap `{other}`"));
            }
            d
        };
        self.err(d);
    }

    fn read_only(&mut self, place: &Place, access: Access, span: Span) {
        if !self.emit {
            return;
        }
        let what = self.describe(place);
        let decl = self.local(place.local);
        let (code, verb) = if access == Access::Write {
            (codes::E0505, "assign to")
        } else {
            (codes::E0512, "lend mutably")
        };
        let why = match decl.kind {
            LocalKind::User(thir::LocalKind::Param(Mode::Borrow)) => {
                format!("`{}` is borrowed, not `mut`", decl.name)
            }
            LocalKind::User(thir::LocalKind::Param(Mode::Take)) => {
                format!("`{}` is taken, and a taken parameter is read-only", decl.name)
            }
            LocalKind::User(thir::LocalKind::Owned { .. }) => {
                format!("`{}` is a `let` binding", decl.name)
            }
            LocalKind::User(thir::LocalKind::Projection { .. }) => {
                format!("`{}` is a read-only projection", decl.name)
            }
            LocalKind::User(thir::LocalKind::ClosureParam) => {
                format!("`{}` is a closure parameter", decl.name)
            }
            LocalKind::TempProjection { role: TempRole::CallResult, .. } => {
                format!("`{}` returns `borrow`, not `mut`", decl.name.trim_end_matches("(..)"))
            }
            LocalKind::Const(_) => format!("`{}` is a constant", decl.name),
            _ => format!("`{}` is read-only", decl.name),
        };
        let mut d = Diagnostic::new(code, span, format!("can't {verb} `{what}`: {why}"));
        d = match decl.kind {
            LocalKind::User(thir::LocalKind::Param(_)) => d.with_help(format!(
                "make the parameter `mut`: `{}: mut ...`, and pass `mut` at the call site",
                decl.name
            )),
            LocalKind::User(thir::LocalKind::Owned { .. }) => {
                let d = d
                    .with_help(format!("declare it with `var {}` to change it", decl.name))
                    .with_secondary(decl.span, "declared here");
                match decl.keyword {
                    Some(k) => d.with_fix("make it `var`", k, "var"),
                    None => d,
                }
            }
            // A `borrow` binding: project with `mut` instead. A pattern's or a loop's name: match
            // or loop with `mut`.
            LocalKind::User(thir::LocalKind::Projection { .. }) => match decl.keyword {
                Some(k) => d
                    .with_help(format!("project it with `mut {} = ...` to change it", decl.name))
                    .with_secondary(decl.span, "declared here")
                    .with_fix("make it `mut`", k, "mut"),
                None => d
                    .with_help("bind it with `match mut place { ... }` or `for mut x in ...` to change it (§6.3)")
                    .with_secondary(decl.span, "declared here"),
            },
            LocalKind::User(thir::LocalKind::ClosureParam) => {
                d.with_help("copy it into a `var` first")
            }
            LocalKind::TempProjection { .. } => {
                d.with_help("call a function that returns `mut` to change it")
            }
            LocalKind::Const(_) => d
                .with_note("a constant lives as long as the program, and every use of it reads the same place (§10)")
                .with_help(format!("copy it into a `var` to change the copy: `var x = {}.clone()`", decl.name)),
            _ => d,
        };
        self.err(d);
    }

    fn not_owned(&mut self, place: &Place, span: Span) {
        if !self.emit {
            return;
        }
        let what = self.describe(place);
        let decl = self.local(place.local);
        let why = match decl.kind {
            LocalKind::User(thir::LocalKind::Param(_)) => {
                format!("`{}` is borrowed by this function, not owned", decl.name)
            }
            LocalKind::User(thir::LocalKind::ClosureParam) => {
                format!("`{}` is a closure parameter", decl.name)
            }
            LocalKind::Const(_) => {
                format!("`{}` is a constant, which lives as long as the program", decl.name)
            }
            _ => format!("`{}` is a projection of another place", decl.name),
        };
        let mut d =
            Diagnostic::new(codes::E0502, span, format!("can't move out of `{what}`: {why}"))
                .with_help(format!("use `{what}.clone()` for a copy you own"));
        if let LocalKind::User(thir::LocalKind::Param(Mode::Borrow)) = decl.kind {
            d = d.with_help(format!(
                "or take ownership in the signature: `{}: take ...`",
                decl.name
            ));
        }
        self.err(d);
    }

    /// `-> borrow T` and `-> mut T` return a place of a `borrow` or `mut` parameter (§6.4).
    fn return_place(&mut self, st: &mut State, at: Point, place: &Place) {
        let mode = self.f.ret_mode;
        let access = if mode == RetMode::Mut { Access::MutBorrow } else { Access::Borrow };
        let ret_span = self.f.block(at.0).term.span;
        self.access(st, at, place, access, ret_span);
        if !self.emit {
            return;
        }
        for t in self.resolve(st, place) {
            let decl = self.local(t.place.local);
            match decl.kind {
                LocalKind::User(thir::LocalKind::Param(Mode::Mut)) => {}
                LocalKind::User(thir::LocalKind::Param(Mode::Borrow)) if mode == RetMode::Borrow => {}
                // A constant lives as long as the program; a `mut` projection of it is
                // reported where it's taken (it's read-only).
                LocalKind::Const(_) => {}
                LocalKind::User(thir::LocalKind::Param(Mode::Borrow)) => self.err(
                    Diagnostic::new(codes::E0512, ret_span, format!("can't return a `mut` projection of `{}`: it's borrowed, not `mut`", decl.name))
                        .with_help(format!("make the parameter `{}: mut ...`", decl.name)),
                ),
                // A value computed for a call's argument: the call's result may point into it.
                LocalKind::Temp | LocalKind::TempVar => self.err(
                    Diagnostic::new(codes::E0508, ret_span, "a projection must come from a `borrow` or `mut` parameter, and this one may point into a temporary")
                        .with_secondary(decl.span, "this temporary is passed to the call, so its result may point into it")
                        .with_note("a call's projection may point into any of its `borrow` and `mut` arguments, and this temporary ends when the function returns (§6.4)")
                        .with_help("pass a parameter's place instead, or return an owned value: `-> T`"),
                ),
                _ => self.err(
                    Diagnostic::new(codes::E0508, ret_span, format!("a projection must come from a `borrow` or `mut` parameter, and `{}` isn't one", decl.name))
                        .with_note("a projection can't outlive what it points into; this function's locals end when it returns (§6.4)")
                        .with_help("return an owned value instead: change the return type to `-> T` and return a copy or a new value"),
                ),
            }
        }
    }
}

/// The places of each local the closure `id`'s body uses (its captures among them), each with
/// whether it's written through: one written and read is written. A closure in it uses all of
/// what it captures.
fn captured_places(body: &Body, id: ClosureId) -> HashMap<Local, Vec<(Place, bool)>> {
    let mut out: HashMap<Local, Vec<(Place, bool)>> = HashMap::new();
    let mut add = |p: &Place, written: bool| {
        let ps = out.entry(p.local).or_default();
        match ps.iter_mut().find(|(q, _)| q == p) {
            Some(e) => e.1 |= written,
            None => ps.push((p.clone(), written)),
        }
    };
    for b in &body.closure_fn(id).blocks {
        for s in &b.stmts {
            match &s.kind {
                StatementKind::Assign(p, r) => {
                    add(p, true);
                    rvalue_places(body, r, &mut add);
                }
                StatementKind::Eval(r) => rvalue_places(body, r, &mut add),
                StatementKind::Bind { place, mutable, .. } => add(place, *mutable),
                StatementKind::Check(p) => add(p, false),
                StatementKind::Live(_) | StatementKind::Dead(_) | StatementKind::Drop(_) => {}
            }
        }
        match &b.term.kind {
            TerminatorKind::If { cond, .. } => operand_place(cond, &mut add),
            TerminatorKind::Return(Some(o)) => operand_place(o, &mut add),
            TerminatorKind::ReturnPlace(p) => add(p, false),
            _ => {}
        }
    }
    out
}

/// [`captured_places`] of an operand: what it reads.
fn operand_place(o: &Operand, add: &mut dyn FnMut(&Place, bool)) {
    if let Some(p) = o.place() {
        add(p, false);
    }
}

/// [`captured_places`] of an rvalue.
fn rvalue_places(body: &Body, r: &Rvalue, add: &mut dyn FnMut(&Place, bool)) {
    let operand = operand_place;
    match r {
        Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::Convert(o) | Rvalue::ArrayRepeat(o, _) => {
            operand(o, add)
        }
        Rvalue::Binary(_, a, b) => {
            operand(a, add);
            operand(b, add);
        }
        Rvalue::Adt { fields: xs, .. }
        | Rvalue::Tuple(xs)
        | Rvalue::Array(xs)
        | Rvalue::Construct(xs) => {
            for o in xs {
                operand(o, add);
            }
        }
        Rvalue::Discriminant(p) | Rvalue::Len(p) => add(p, false),
        Rvalue::Call(Call { args, .. }) | Rvalue::BorrowStruct { fields: args, .. } => {
            for a in args {
                match a {
                    Arg::Borrow(p, _) => add(p, false),
                    Arg::Mut(p, _) => add(p, true),
                    Arg::Take(o) => operand(o, add),
                }
            }
        }
        Rvalue::Closure(inner) => {
            for &(l, w) in &body.closures[inner.0 as usize].captures {
                add(&Place::local(l), w);
            }
        }
        Rvalue::Dispatch(d) => {
            for o in &d.groups {
                operand(o, add);
            }
            for (_, p, _) in &d.args {
                add(p, false);
            }
            if let Some((p, _)) = &d.indirect {
                add(p, false);
            }
        }
        Rvalue::Draw(d) => {
            operand(&d.vertices, add);
            operand(&d.instances, add);
            for (_, _, p, _) in &d.args {
                add(p, false);
            }
            if let Some((p, _)) = &d.indirect {
                add(p, false);
            }
        }
        Rvalue::FnRef(..)
        | Rvalue::Const(_)
        | Rvalue::Text(_)
        | Rvalue::Embed(_)
        | Rvalue::ConstParam(_) => {}
    }
}

/// W0006: a `var` that never changes after it's bound: nothing assigns it again, writes
/// through it, lends it `mut`, or captures it to write. It's a `let`.
fn unchanged_vars(body: &Body) -> Vec<Diagnostic> {
    let n = body.locals.len();
    let mut changed = vec![false; n];
    // Assignments of a whole local: its binding is one.
    let mut whole = vec![0u32; n];
    let mark = |p: &Place, changed: &mut Vec<bool>| changed[p.local.index()] = true;
    for f in &body.fns {
        for b in &f.blocks {
            for s in &b.stmts {
                match &s.kind {
                    StatementKind::Assign(place, r) => {
                        if place.proj.is_empty() {
                            whole[place.local.index()] += 1;
                        } else {
                            mark(place, &mut changed);
                        }
                        rvalue_writes(r, &mut |p| mark(p, &mut changed));
                    }
                    StatementKind::Eval(r) => rvalue_writes(r, &mut |p| mark(p, &mut changed)),
                    StatementKind::Bind { place, mutable: true, .. } => mark(place, &mut changed),
                    _ => {}
                }
            }
        }
    }
    for c in &body.closures {
        for &(l, writes) in &c.captures {
            if writes {
                changed[l.index()] = true;
            }
        }
    }
    let mut out = Vec::new();
    for (i, d) in body.locals.iter().enumerate() {
        let var =
            matches!(d.kind, LocalKind::User(crate::thir::LocalKind::Owned { mutable: true }));
        if !var || changed[i] || whole[i] > 1 || d.name.starts_with('_') {
            continue;
        }
        let Some(kw) = d.keyword else { continue };
        out.push(
            Diagnostic::new(
                codes::W0006,
                d.span,
                format!("`{}` is a `var`, but it never changes", d.name),
            )
            .with_note("`let` owns a value that stays as it is; `var` says it will change (§6.3)")
            .with_fix("make it a `let`", kw, "let"),
        );
    }
    out
}

/// Calls `write` on each place `r` lends `mut` or hands to the GPU (which may write it).
fn rvalue_writes(r: &Rvalue, write: &mut impl FnMut(&Place)) {
    match r {
        Rvalue::Call(Call { args, .. }) | Rvalue::BorrowStruct { fields: args, .. } => {
            for a in args {
                if let Arg::Mut(p, _) = a {
                    write(p);
                }
            }
        }
        Rvalue::Dispatch(d) => d.args.iter().for_each(|(_, p, _)| write(p)),
        Rvalue::Draw(d) => d.args.iter().for_each(|(_, _, p, _)| write(p)),
        _ => {}
    }
}

/// W0001: a local bound and never used. Parameters, `_` and names starting with `_` aren't
/// reported.
fn unused_locals(p: &Program, body: &Body) -> Vec<Diagnostic> {
    let n = body.locals.len();
    let mut used = vec![false; n];
    let mut bound = vec![false; n];
    // How many assignments give each local a value: one is its binding's.
    let mut assigned = vec![0u32; n];
    let mut uses = Vec::new();
    for f in &body.fns {
        for b in &f.blocks {
            for s in &b.stmts {
                stmt_uses_defs(p, body, s, &mut uses);
                match &s.kind {
                    StatementKind::Live(l) => bound[l.index()] = true,
                    StatementKind::Assign(place, _) => assigned[place.local.index()] += 1,
                    _ => {}
                }
            }
            term_uses(&b.term, &mut uses);
            for u in uses.drain(..) {
                used[u.index()] = true;
            }
        }
    }
    for c in &body.closures {
        for (l, _) in &c.captures {
            used[l.index()] = true;
        }
    }
    let mut out = Vec::new();
    for (i, d) in body.locals.iter().enumerate() {
        if !bound[i] || used[i] || d.name.starts_with('_') {
            continue;
        }
        if !matches!(d.kind, LocalKind::User(_)) {
            continue;
        }
        let name = &d.name;
        let warning = Diagnostic::new(codes::W0001, d.span, format!("`{name}` is never used"));
        let rename = format!("rename it `_{name}`");
        out.push(if d.shorthand {
            warning.with_fix(rename, d.span, format!("{name}: _{name}"))
        } else if assigned[i] > 1 {
            // Its assignments name it too, and only its binding has a span here.
            warning.with_help(format!(
                "{rename} where it's bound and where it's assigned, or remove it"
            ))
        } else {
            warning.with_fix(rename, d.span.shrink_to_start(), "_")
        });
    }
    out
}
