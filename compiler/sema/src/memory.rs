//! The memory checker (language.md §6): parameter modes, moves, projections and exclusivity,
//! checked within one function at a time; signatures say everything about callees.
//!
//! - **Places** are a local and a path of fields, vector components and elements (§6.5). Two
//!   places overlap when one is a prefix of the other; every element of a container overlaps
//!   every other.
//! - **Moves** are flow-sensitive: a value moved on some path is "possibly moved" after the
//!   join; a move inside a loop of a value from outside it is caught on the second iteration.
//! - **Loans** come from projections (`let x = place`, `mut x = place`, `for x in array`, a
//!   call returning a projection), closures (their captures), and call arguments (for the call).
//!   A loan held by a local lives until that local's last use; a use inside a loop keeps it
//!   alive to the loop's end (a simple non-lexical lifetime).
//! - **The rule:** while a `mut` loan is live, no other access may touch an overlapping place;
//!   while a shared loan is live, nothing may write, move or lend mutably an overlapping place.
//!   Access through the loan's own holder is the loan's own use.

use crate::check::root_local;
use crate::defs::*;
use crate::program::Program;
use crate::thir::*;
use crate::traits;
use crate::ty::*;
use wrela_diag::{Diagnostic, Span, codes};

#[derive(Clone, Debug, PartialEq)]
enum Proj {
    Field(u32),
    Comp(u8),
    Index,
}

#[derive(Clone, Debug, PartialEq)]
struct Place {
    root: LocalId,
    path: Vec<Proj>,
}

fn overlaps(a: &Place, b: &Place) -> bool {
    a.root == b.root && a.path.iter().zip(&b.path).all(|(x, y)| x == y)
}

#[derive(Clone, Debug)]
struct Loan {
    place: Place,
    mutable: bool,
    /// The local holding the loan, or `None` for a call's argument.
    holder: Option<LocalId>,
    /// For a call's argument: the call's frame.
    call: Option<u32>,
    from: u32,
    until: u32,
    span: Span,
}

#[derive(Clone, Debug)]
struct Moved {
    place: Place,
    span: Span,
    maybe: bool,
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

/// How an expression's value is used by its context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Use {
    /// The value is read: a `Copy` place is copied; any other place must not be moved here.
    Read,
    /// The value is consumed: a non-`Copy` place needs `take` (or is a local being returned).
    Move,
    /// The value is moved out implicitly if it's a whole local the function owns (`return x`).
    Return,
    /// Borrowed for a call, or projected by a binding.
    Borrow,
    MutBorrow,
}

/// Moves along the current path; `None` when the path can't continue (after `return`).
type MoveState = Option<Vec<Moved>>;

fn merge(a: MoveState, b: MoveState) -> MoveState {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(a), Some(b)) => {
            let mut out: Vec<Moved> = Vec::new();
            for m in &a {
                let in_b = b.iter().any(|n| n.place == m.place);
                out.push(Moved { place: m.place.clone(), span: m.span, maybe: m.maybe || !in_b });
            }
            for n in &b {
                if !a.iter().any(|m| m.place == n.place) {
                    out.push(Moved { place: n.place.clone(), span: n.span, maybe: true });
                }
            }
            Some(out)
        }
    }
}

pub fn check_fn(p: &mut Program, f: FnId, body: &Body) -> Vec<Diagnostic> {
    let def = p.func(f).clone();
    let mut diags = Vec::new();
    let mut w = Walker::new(p, body, Some(def.ret_mode), None);
    w.run_value(&body.value);
    diags.extend(w.diags);
    for (i, c) in body.closures.iter().enumerate() {
        let mut w = Walker::new(p, body, None, Some(ClosureId(i as u32)));
        w.run_value(&c.body);
        diags.extend(w.diags);
    }
    diags
}

struct Walker<'a> {
    p: &'a mut Program,
    body: &'a Body,
    diags: Vec<Diagnostic>,
    /// The function's return mode (`None` inside a closure, whose returns are values).
    ret_mode: Option<RetMode>,
    closure: Option<ClosureId>,
    liveness: bool,
    pos: u32,
    live_end: Vec<u32>,
    decl_depth: Vec<u32>,
    loops: Vec<LoopFrame>,
    loans: Vec<Loan>,
    moves: MoveState,
    next_call: u32,
    calls: Vec<u32>,
}

/// A `for` loop's variable, and the loan its projections take on the array (with whether it's
/// mutable and where).
type LoopVar = (LocalId, Option<(Place, bool, Span)>);

#[derive(Default)]
struct LoopFrame {
    used: Vec<LocalId>,
    breaks: MoveState,
    has_break: bool,
}

impl<'a> Walker<'a> {
    fn new(
        p: &'a mut Program,
        body: &'a Body,
        ret_mode: Option<RetMode>,
        closure: Option<ClosureId>,
    ) -> Walker<'a> {
        let n = body.locals.len();
        Walker {
            p,
            body,
            diags: Vec::new(),
            ret_mode,
            closure,
            liveness: true,
            pos: 0,
            live_end: vec![0; n],
            decl_depth: vec![0; n],
            loops: Vec::new(),
            loans: Vec::new(),
            moves: Some(Vec::new()),
            next_call: 0,
            calls: Vec::new(),
        }
    }

    /// Two passes over the same tree, in the same order: liveness, then the checks.
    fn run_value(&mut self, value: &Expr) {
        self.liveness = true;
        let u = if self.closure.is_some() { Use::Move } else { self.ret_use() };
        self.expr(value, u);
        self.liveness = false;
        self.pos = 0;
        self.next_call = 0;
        self.loans.clear();
        self.moves = Some(Vec::new());
        if self.closure.is_none() {
            self.check_projection_return(value);
        }
        self.expr(value, u);
    }

    fn ret_use(&self) -> Use {
        match self.ret_mode {
            Some(RetMode::Owned) | None => Use::Return,
            Some(RetMode::Borrow) => Use::Borrow,
            Some(RetMode::Mut) => Use::MutBorrow,
        }
    }

    fn tick(&mut self) -> u32 {
        self.pos += 1;
        self.pos
    }

    fn err(&mut self, d: Diagnostic) {
        if !self.liveness {
            self.diags.push(d);
        }
    }

    fn local(&self, l: LocalId) -> &LocalDecl {
        &self.body.locals[l.index()]
    }

    fn name(&self, l: LocalId) -> String {
        self.local(l).name.clone()
    }

    fn is_copy(&mut self, t: TyId) -> bool {
        traits::implements_builtin(self.p, t, Lang::Copy)
    }

    /// Whether `l` is outside the closure being checked (a capture).
    fn is_capture(&self, l: LocalId) -> bool {
        match self.closure {
            Some(c) => {
                self.local(l).closure != Some(c)
                    && !self.body.closures[c.0 as usize].params.contains(&l)
            }
            None => false,
        }
    }

    /// Records a use of a local for liveness.
    fn use_local(&mut self, l: LocalId) {
        if self.liveness {
            let p = self.pos;
            if self.live_end[l.index()] < p {
                self.live_end[l.index()] = p;
            }
            for f in self.loops.iter_mut() {
                f.used.push(l);
            }
        }
    }

    fn declare(&mut self, l: LocalId) {
        self.decl_depth[l.index()] = self.loops.len() as u32;
        // A binding is a new value: in a loop's next pass, what the last one moved out of it is
        // gone with it.
        if let Some(m) = &mut self.moves {
            m.retain(|x| x.place.root != l);
        }
        let p = self.tick();
        if self.liveness && self.live_end[l.index()] < p {
            self.live_end[l.index()] = p;
        }
    }

    // ---- places ----------------------------------------------------------------------------

    /// The place an expression names, visiting index expressions along the way. `None` for
    /// values that aren't rooted at a local (temporaries, constants, call results).
    fn place(&mut self, e: &Expr) -> Option<Place> {
        match &e.kind {
            ExprKind::Local(l) => Some(Place { root: *l, path: Vec::new() }),
            ExprKind::Field(b, i) => {
                let mut p = self.place(b)?;
                p.path.push(Proj::Field(*i));
                Some(p)
            }
            ExprKind::Swizzle(b, comps) => {
                let mut p = self.place(b)?;
                if comps.len() == 1 {
                    p.path.push(Proj::Comp(comps[0]));
                }
                Some(p)
            }
            ExprKind::Index(b, i) => {
                let mut p = self.place(b)?;
                self.expr(i, Use::Read);
                p.path.push(Proj::Index);
                Some(p)
            }
            _ => None,
        }
    }

    fn describe(&self, p: &Place) -> String {
        let mut s = self.name(p.root);
        let mut ty = self.local(p.root).ty;
        for proj in &p.path {
            match proj {
                Proj::Field(i) => {
                    let name = match self.p.types.kind(ty).clone() {
                        TyKind::Adt(a, args) => {
                            let f = self
                                .p
                                .adt(a)
                                .fields()
                                .get(*i as usize)
                                .map(|f| (f.name.clone(), f.ty));
                            if let Some((n, t)) = f {
                                let subst = Subst::from_pairs(&self.p.adt(a).generics, &args);
                                let _ = subst;
                                ty = t;
                                n
                            } else {
                                i.to_string()
                            }
                        }
                        TyKind::Tuple(ts) => {
                            ty = ts.get(*i as usize).copied().unwrap_or(ty);
                            i.to_string()
                        }
                        _ => i.to_string(),
                    };
                    s.push('.');
                    s.push_str(&name);
                }
                Proj::Comp(c) => {
                    s.push('.');
                    s.push(['x', 'y', 'z', 'w'][*c as usize]);
                }
                Proj::Index => s.push_str("[_]"),
            }
        }
        s
    }

    /// Checks an access to `place` and applies it (a move, or a new loan).
    fn access(
        &mut self,
        place: &Place,
        access: Access,
        span: Span,
        holder: Option<LocalId>,
        until: u32,
    ) {
        let pos = self.tick();
        self.use_local(place.root);
        if self.liveness {
            return;
        }
        let what = self.describe(place);
        // 1. Moved?
        if let Some(moves) = &self.moves {
            let hit = moves.iter().find(|m| overlaps(&m.place, place)).cloned();
            if let Some(m) = hit {
                let reinit = access == Access::Write && m.place.path.len() >= place.path.len();
                if !reinit {
                    let moved = self.describe(&m.place);
                    let d = if m.maybe {
                        Diagnostic::new(
                            codes::E0516,
                            span,
                            format!("`{what}` might have been moved already"),
                        )
                        .with_secondary(m.span, format!("`{moved}` is moved here on some paths"))
                        .with_help("move it on every path, or `.clone()` it where it's moved")
                    } else {
                        Diagnostic::new(
                            codes::E0500,
                            span,
                            format!("`{what}` was moved, so it can't be used here"),
                        )
                        .with_secondary(m.span, format!("`{moved}` is moved here"))
                        .with_help(format!(
                            "pass `{moved}.clone()` where it's moved if you still need it here"
                        ))
                    };
                    self.err(d);
                    return;
                }
            }
        }
        // 2. Mutability and ownership.
        let decl = self.local(place.root).clone();
        let capture = self.is_capture(place.root);
        match access {
            Access::Write | Access::MutBorrow => {
                let writable = if capture {
                    true // a capture that's written is captured mutably; its owner was checked
                } else {
                    match decl.kind {
                        LocalKind::Owned { mutable } | LocalKind::Projection { mutable } => mutable,
                        LocalKind::Param(m) => m == Mode::Mut,
                        LocalKind::ClosureParam => false,
                    }
                };
                if !writable {
                    let (code, verb) = if access == Access::Write {
                        (codes::E0505, "assign to")
                    } else {
                        (codes::E0512, "lend mutably")
                    };
                    let mut d = Diagnostic::new(
                        code,
                        span,
                        format!("can't {verb} `{what}`: {}", self.why_read_only(&decl)),
                    );
                    d = match decl.kind {
                        LocalKind::Param(_) => d.with_help(format!("make the parameter `mut`: `{}: mut ...`, and pass `mut` at the call site", decl.name)),
                        LocalKind::Owned { .. } => {
                            let d = d
                                .with_help(format!("declare it with `var {}` to change it", decl.name))
                                .with_secondary(decl.span, "declared here");
                            match decl.keyword {
                                Some(k) => d.with_fix("make it `var`", k, "var"),
                                None => d,
                            }
                        }
                        LocalKind::Projection { .. } => d.with_help(format!("project it with `mut {} = ...` to change it", decl.name)).with_secondary(decl.span, "declared here"),
                        LocalKind::ClosureParam => d.with_help("copy it into a `var` first"),
                    };
                    self.err(d);
                }
            }
            Access::Move => {
                if capture {
                    self.err(
                        Diagnostic::new(
                            codes::E0502,
                            span,
                            format!("can't move `{what}` out of the closure's surroundings"),
                        )
                        .with_note("a closure borrows what it uses; it doesn't own it (§6.7)")
                        .with_help(format!("use `{what}.clone()`")),
                    );
                    return;
                }
                let owned =
                    matches!(decl.kind, LocalKind::Owned { .. } | LocalKind::Param(Mode::Take));
                if !owned {
                    let mut d = Diagnostic::new(
                        codes::E0502,
                        span,
                        format!("can't move out of `{what}`: {}", self.why_not_owned(&decl)),
                    )
                    .with_help(format!("use `{what}.clone()` for a copy you own"));
                    if let LocalKind::Param(Mode::Borrow) = decl.kind {
                        d = d.with_help(format!(
                            "or take ownership in the signature: `{}: take ...`",
                            decl.name
                        ));
                    }
                    self.err(d);
                    return;
                }
                if place.path.contains(&Proj::Index) {
                    self.err(
                        Diagnostic::new(
                            codes::E0502,
                            span,
                            format!("can't move out of `{what}`: it's an element of an array"),
                        )
                        .with_note(
                            "an element can't be moved out on its own; the array would have a hole",
                        )
                        .with_help(format!("use `{what}.clone()`")),
                    );
                    return;
                }
            }
            _ => {}
        }
        // 3. Loans.
        let conflicts: Vec<Loan> = self
            .loans
            .iter()
            .filter(|l| l.from < pos && pos <= l.until)
            .filter(|l| l.holder != Some(place.root))
            .filter(|l| overlaps(&l.place, place))
            .filter(|l| {
                l.mutable || matches!(access, Access::Write | Access::Move | Access::MutBorrow)
            })
            .cloned()
            .collect();
        if let Some(l) = conflicts.first() {
            let other = self.describe(&l.place);
            let d = match l.holder {
                Some(h) => {
                    let hn = self.name(h);
                    let (msg, note) = if l.mutable {
                        (
                            format!(
                                "`{what}` overlaps `{other}`, which `{hn}` is borrowing mutably"
                            ),
                            format!("`{hn}` is used later, so the borrow is still live"),
                        )
                    } else {
                        (
                            format!("`{what}` can't be changed while `{hn}` borrows `{other}`"),
                            format!("`{hn}` is used later, so the borrow is still live"),
                        )
                    };
                    let code = if l.mutable { codes::E0506 } else { codes::E0507 };
                    let mut d = Diagnostic::new(code, span, msg)
                        .with_secondary(l.span, format!("`{hn}` borrows `{other}` here"))
                        .with_note(note);
                    if l.mutable
                        && matches!(access, Access::Borrow | Access::Read)
                        && place.path.len() < l.place.path.len()
                    {
                        d = d
                            .with_help(format!("pass only the parts that don't overlap `{other}`"));
                    }
                    d
                }
                None => Diagnostic::new(
                    codes::E0513,
                    span,
                    match (what == other, l.mutable) {
                        (true, true) => format!("`{what}` is already passed `mut` in this call"),
                        (true, false) => format!("`{what}` is already passed in this call"),
                        (false, true) => format!(
                            "`{what}` overlaps `{other}`, which this call already takes mutably"
                        ),
                        (false, false) => {
                            format!("`{what}` overlaps `{other}`, which this call already takes")
                        }
                    },
                )
                .with_secondary(l.span, format!("`{other}` is passed here"))
                .with_note("in one call, arguments can't overlap when one of them is `mut` (§6.5)"),
            };
            self.err(d);
        }
        // 4. Apply.
        match access {
            Access::Move => {
                if let Some(m) = &mut self.moves {
                    m.push(Moved { place: place.clone(), span, maybe: false });
                }
            }
            Access::Write => {
                // Assigning a whole place reinitializes what was moved out of it.
                if let Some(m) = &mut self.moves {
                    m.retain(|x| {
                        !(x.place.root == place.root && x.place.path.starts_with(&place.path))
                    });
                }
            }
            Access::Borrow | Access::MutBorrow => {
                if until > pos {
                    let call = if holder.is_none() { self.calls.last().copied() } else { None };
                    self.loans.push(Loan {
                        place: place.clone(),
                        mutable: access == Access::MutBorrow,
                        holder,
                        call,
                        from: pos,
                        until,
                        span,
                    });
                }
            }
            Access::Read => {}
        }
    }

    fn why_read_only(&self, d: &LocalDecl) -> String {
        match d.kind {
            LocalKind::Param(Mode::Borrow) => format!("`{}` is borrowed, not `mut`", d.name),
            LocalKind::Param(Mode::Take) => {
                format!("`{}` is taken, and a taken parameter is read-only", d.name)
            }
            LocalKind::Owned { .. } => format!("`{}` is a `let` binding", d.name),
            LocalKind::Projection { .. } => format!("`{}` is a read-only projection", d.name),
            LocalKind::ClosureParam => format!("`{}` is a closure parameter", d.name),
            LocalKind::Param(Mode::Mut) => String::new(),
        }
    }

    fn why_not_owned(&self, d: &LocalDecl) -> String {
        match d.kind {
            LocalKind::Param(_) => format!("`{}` is borrowed by this function, not owned", d.name),
            LocalKind::Projection { .. } => {
                format!("`{}` is a projection of another place", d.name)
            }
            LocalKind::ClosureParam => format!("`{}` is a closure parameter", d.name),
            LocalKind::Owned { .. } => String::new(),
        }
    }

    /// A place or a value, used as `u` says.
    fn expr(&mut self, e: &Expr, u: Use) {
        // A place expression?
        if matches!(
            e.kind,
            ExprKind::Local(_) | ExprKind::Field(..) | ExprKind::Swizzle(..) | ExprKind::Index(..)
        ) && root_local(e).is_some()
            && let Some(place) = self.place(e)
        {
            self.place_use(&place, e, u);
            return;
        }
        self.value(e, u);
    }

    fn place_use(&mut self, place: &Place, e: &Expr, u: Use) {
        let copy = self.is_copy(e.ty);
        match u {
            Use::Read => self.access(place, Access::Read, e.span, None, 0),
            Use::Borrow => {
                let until = self.call_until();
                self.access(place, Access::Borrow, e.span, None, until)
            }
            Use::MutBorrow => {
                let until = self.call_until();
                self.access(place, Access::MutBorrow, e.span, None, until)
            }
            Use::Move | Use::Return => {
                if copy {
                    self.access(place, Access::Read, e.span, None, 0);
                    return;
                }
                let whole_owned_local = place.path.is_empty()
                    && matches!(
                        self.local(place.root).kind,
                        LocalKind::Owned { .. } | LocalKind::Param(Mode::Take)
                    );
                if u == Use::Return && whole_owned_local {
                    self.access(place, Access::Move, e.span, None, 0);
                    return;
                }
                let what = self.describe(place);
                self.access(place, Access::Read, e.span, None, 0);
                let owned = matches!(
                    self.local(place.root).kind,
                    LocalKind::Owned { .. } | LocalKind::Param(Mode::Take)
                ) && !self.is_capture(place.root);
                let shown = self.p.display_ty(e.ty);
                let d = if owned && !place.path.contains(&Proj::Index) {
                    Diagnostic::new(
                        codes::E0501,
                        e.span,
                        format!("moving `{what}` out of a named place is written `take {what}`"),
                    )
                    .with_note(format!("`{shown}` isn't `Copy`, so using it here moves it (§6.1)"))
                    .with_fix(
                        format!("move it: `take {what}`"),
                        e.span.shrink_to_start(),
                        "take ",
                    )
                } else {
                    Diagnostic::new(
                        codes::E0502,
                        e.span,
                        format!(
                            "can't move `{what}` here: {}",
                            if place.path.contains(&Proj::Index) {
                                "it's an element of an array".to_string()
                            } else {
                                self.why_not_owned(&self.local(place.root).clone())
                            }
                        ),
                    )
                    .with_note(format!("`{shown}` isn't `Copy`, so using it here would move it"))
                    .with_fix(
                        format!("copy it: `{what}.clone()`"),
                        e.span.shrink_to_end(),
                        ".clone()",
                    )
                };
                self.err(d);
            }
        }
    }

    /// The end of the innermost call's arguments, for a loan that lasts the call.
    fn call_until(&self) -> u32 {
        if self.calls.is_empty() { 0 } else { u32::MAX }
    }

    fn begin_call(&mut self) -> u32 {
        let id = self.next_call;
        self.next_call += 1;
        self.calls.push(id);
        id
    }

    fn end_call(&mut self, id: u32) {
        let pos = self.tick();
        for l in &mut self.loans {
            if l.call == Some(id) && l.until == u32::MAX {
                l.until = pos;
            }
        }
        self.calls.pop();
    }

    // ---- values ----------------------------------------------------------------------------

    fn value(&mut self, e: &Expr, u: Use) {
        match &e.kind {
            ExprKind::Lit(_)
            | ExprKind::Const(_)
            | ExprKind::Break
            | ExprKind::Continue
            | ExprKind::Error
            | ExprKind::FnRef(..) => {
                if matches!(e.kind, ExprKind::Break) {
                    let s = self.moves.clone();
                    if let Some(f) = self.loops.last_mut() {
                        f.has_break = true;
                        f.breaks = merge(f.breaks.take(), s);
                    }
                    self.moves = None;
                } else if matches!(e.kind, ExprKind::Continue) {
                    self.moves = None;
                }
            }
            ExprKind::Local(_)
            | ExprKind::Field(..)
            | ExprKind::Swizzle(..)
            | ExprKind::Index(..) => {
                // A field or element of a temporary: evaluate the temporary.
                match &e.kind {
                    ExprKind::Field(b, _) | ExprKind::Swizzle(b, _) => self.expr(b, Use::Read),
                    ExprKind::Index(b, i) => {
                        self.expr(b, Use::Read);
                        self.expr(i, Use::Read);
                    }
                    _ => {}
                }
            }
            ExprKind::Unary(_, x) | ExprKind::Convert(x) => self.expr(x, Use::Read),
            ExprKind::Binary(_, a, b) => {
                self.expr(a, Use::Read);
                self.expr(b, Use::Read);
            }
            ExprKind::Call(c) => self.call(c, e),
            ExprKind::Adt { fields, .. } => fields.iter().for_each(|f| self.expr(f, Use::Move)),
            ExprKind::Tuple(xs) | ExprKind::Array(xs) | ExprKind::Construct(xs) => {
                xs.iter().for_each(|x| self.expr(x, Use::Move))
            }
            ExprKind::ArrayRepeat(x, _) => self.expr(x, Use::Read),
            ExprKind::Block(b) => self.block(b, u),
            ExprKind::If { cond, then, else_ } => {
                self.expr(cond, Use::Read);
                let entry = self.moves.clone();
                self.block(then, u);
                let after_then = std::mem::replace(&mut self.moves, entry);
                if let Some(x) = else_ {
                    self.expr(x, u);
                }
                let after_else = self.moves.take();
                self.moves = merge(after_then, after_else);
            }
            ExprKind::Match { scrutinee, arms } => {
                let su = if scrutinee.is_place() && !self.is_copy(scrutinee.ty) {
                    Use::Borrow
                } else {
                    Use::Read
                };
                // The arms' bindings project into the scrutinee: a shared loan while they live.
                let holders: Vec<LocalId> = arms
                    .iter()
                    .flat_map(|a| bindings(&a.pat))
                    .filter(|l| matches!(self.local(*l).kind, LocalKind::Projection { .. }))
                    .collect();
                if su == Use::Borrow
                    && let Some(place) = self.place(scrutinee)
                {
                    let until = holders.iter().map(|h| self.live_end[h.index()]).max().unwrap_or(0);
                    self.access(
                        &place,
                        Access::Borrow,
                        scrutinee.span,
                        holders.first().copied(),
                        until,
                    );
                } else {
                    self.expr(scrutinee, su);
                }
                let entry = self.moves.clone();
                let mut out: MoveState = None;
                for a in arms {
                    self.moves = entry.clone();
                    for l in bindings(&a.pat) {
                        self.declare(l);
                    }
                    if let Some(g) = &a.guard {
                        self.expr(g, Use::Read);
                    }
                    self.expr(&a.body, u);
                    out = merge(out, self.moves.take());
                }
                self.moves = if arms.is_empty() { entry } else { out };
            }
            ExprKind::Closure(id) => self.closure(*id, None, e.span),
            ExprKind::Take(inner) => self.take(inner),
            ExprKind::MutArg(inner) => {
                if let Some(place) = self.place(inner).filter(|_| root_local(inner).is_some()) {
                    let until = self.call_until();
                    self.access(&place, Access::MutBorrow, inner.span, None, until);
                } else {
                    self.expr(inner, Use::Read);
                }
            }
            ExprKind::Return(v) => {
                if let Some(v) = v {
                    let ru = if self.closure.is_some() { Use::Move } else { self.ret_use() };
                    if !self.liveness && self.closure.is_none() {
                        self.check_projection_return(v);
                    }
                    self.expr(v, ru);
                }
                self.moves = None;
            }
            ExprKind::Dispatch(d) => {
                for g in &d.groups {
                    self.expr(g, Use::Read);
                }
                let call = self.begin_call();
                for (_, a) in &d.args {
                    self.expr(a, Use::Borrow);
                }
                self.end_call(call);
            }
            ExprKind::Draw(d) => {
                self.expr(&d.vertices, Use::Read);
                self.expr(&d.instances, Use::Read);
                let call = self.begin_call();
                for (_, a) in &d.args {
                    self.expr(a, Use::Borrow);
                }
                self.end_call(call);
            }
        }
    }

    /// `take place`, or `take recv.method()` for a `take self` method.
    fn take(&mut self, inner: &Expr) {
        if let ExprKind::Call(c) = &inner.kind
            && c.receiver
            && c.modes.first() == Some(&Mode::Take)
        {
            self.call_with_taken_receiver(c, inner);
            return;
        }
        match self.place(inner).filter(|_| root_local(inner).is_some()) {
            Some(place) => {
                if self.is_copy(inner.ty) {
                    self.access(&place, Access::Read, inner.span, None, 0);
                } else {
                    self.access(&place, Access::Move, inner.span, None, 0);
                }
            }
            None => self.expr(inner, Use::Move),
        }
    }

    fn call(&mut self, c: &Call, e: &Expr) {
        if c.receiver && c.modes.first() == Some(&Mode::Take) {
            // A consuming method on a named place must be marked `take x.m()`.
            if let Some(recv) = c.args.first()
                && recv.is_place()
                && root_local(recv).is_some()
                && !self.is_copy(recv.ty)
            {
                let what = self.place(recv).map(|p| self.describe(&p)).unwrap_or_default();
                self.err(
                    Diagnostic::new(codes::E0501, e.span, format!("this method takes `{what}`, so the call is written `take {what}...`"))
                        .with_note("a consuming method moves its receiver; moving out of a named place is marked (§6.2)")
                        .with_fix("mark the move", e.span.shrink_to_start(), "take "),
                );
            }
        }
        self.call_args(c, false);
    }

    fn call_with_taken_receiver(&mut self, c: &Call, _e: &Expr) {
        self.call_args(c, true);
    }

    fn call_args(&mut self, c: &Call, receiver_taken: bool) {
        let id = self.begin_call();
        if let Callee::Local(l) = c.callee {
            self.use_local(l);
        }
        for (i, (a, &m)) in c.args.iter().zip(&c.modes).enumerate() {
            let is_recv = c.receiver && i == 0;
            let u = match m {
                Mode::Borrow => Use::Borrow,
                Mode::Mut => {
                    if is_recv {
                        Use::MutBorrow
                    } else {
                        // Marked `mut x` (MutArg handles it), or already reported unmarked.
                        Use::Borrow
                    }
                }
                Mode::Take => Use::Move,
            };
            if is_recv && receiver_taken {
                match self.place(a).filter(|_| root_local(a).is_some()) {
                    Some(place) => self.access(&place, Access::Move, a.span, None, 0),
                    None => self.expr(a, Use::Move),
                }
                continue;
            }
            if is_recv && m == Mode::Take {
                // Reported in `call`; read it so later uses are still checked.
                self.expr(a, Use::Read);
                continue;
            }
            if let ExprKind::Closure(cid) = a.kind {
                self.closure(cid, None, a.span);
                continue;
            }
            self.expr(a, u);
        }
        self.end_call(id);
    }

    /// A closure's creation: its captures are borrowed (mutably if written) while the closure
    /// lives: until its holder's last use, or for the call it's passed to.
    fn closure(&mut self, id: ClosureId, holder: Option<LocalId>, span: Span) {
        let caps = self.body.closures[id.0 as usize].captures.clone();
        let until = match holder {
            Some(h) => self.live_end[h.index()],
            None => self.call_until(),
        };
        for (l, written) in caps {
            let place = Place { root: l, path: Vec::new() };
            let access = if written { Access::MutBorrow } else { Access::Borrow };
            self.access(&place, access, span, holder, until);
        }
    }

    // ---- blocks and statements -------------------------------------------------------------

    fn block(&mut self, b: &Block, u: Use) {
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(t) = &b.tail {
            self.expr(t, u);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Bind { pat, init } => self.bind(pat, init),
            StmtKind::Assign { place, op, value } => {
                self.expr(value, if op.is_some() { Use::Read } else { Use::Move });
                match self.place(place).filter(|_| root_local(place).is_some()) {
                    Some(p) => {
                        if op.is_some() {
                            self.access(&p, Access::Read, place.span, None, 0);
                        }
                        self.access(&p, Access::Write, place.span, None, 0);
                    }
                    None => {
                        // Through a projection returned by a call: writable if it's `mut`.
                        self.expr(place, Use::Read);
                        if let ExprKind::Call(c) = &innermost(place).kind
                            && c.ret_mode != RetMode::Mut
                        {
                            self.err(
                                Diagnostic::new(
                                    codes::E0505,
                                    place.span,
                                    "can't assign through a read-only projection",
                                )
                                .with_note("the function returns `borrow`, not `mut`"),
                            );
                        }
                    }
                }
            }
            StmtKind::Expr(e) => self.expr(e, Use::Read),
            StmtKind::While { cond, body } => self.loop_(Some(cond), body, None),
            StmtKind::Loop { body } => self.loop_(None, body, None),
            StmtKind::ForRange { var, start, end, body, .. } => {
                self.expr(start, Use::Read);
                self.expr(end, Use::Read);
                self.loop_(None, body, Some((*var, None)));
            }
            StmtKind::ForEach { var, array, mutable, body } => {
                // The loop variable projects each element: a loan on the array for the loop.
                let projection = matches!(self.local(*var).kind, LocalKind::Projection { .. });
                let place = self.place(array).filter(|_| root_local(array).is_some());
                let loan = match (place, projection) {
                    (Some(p), true) => Some((p, *mutable)),
                    (Some(p), false) => {
                        self.access(&p, Access::Read, array.span, None, 0);
                        None
                    }
                    (None, _) => {
                        self.expr(array, Use::Read);
                        None
                    }
                };
                self.loop_(None, body, Some((*var, loan.map(|(p, m)| (p, m, array.span)))));
            }
        }
    }

    fn bind(&mut self, pat: &Pat, init: &Expr) {
        let locals = bindings(pat);
        let single = match pat.kind {
            PatKind::Bind(l) => Some(l),
            _ => None,
        };
        let kind = single.map(|l| self.local(l).kind);
        match (single, kind) {
            // `let x = place` (not Copy): a read-only projection; `mut x = place`: mutable.
            (Some(l), Some(LocalKind::Projection { mutable })) => {
                if let Some(place) = self.place(init).filter(|_| root_local(init).is_some()) {
                    let until = self.live_end[l.index()];
                    let access = if mutable { Access::MutBorrow } else { Access::Borrow };
                    self.access(&place, access, init.span, Some(l), until);
                } else if let ExprKind::Call(c) = &init.kind
                    && c.ret_mode != RetMode::Owned
                {
                    // A projection returned by a call borrows the call's borrowed and `mut`
                    // arguments for as long as the binding lives.
                    self.call_projection(c, l, mutable && c.ret_mode == RetMode::Mut);
                } else if mutable {
                    self.err(
                        Diagnostic::new(
                            codes::E0512,
                            init.span,
                            "`mut x = ...` projects a place, and this is a temporary",
                        )
                        .with_help("to own a new value you can change, write `var x = ...`"),
                    );
                    self.expr(init, Use::Read);
                } else {
                    self.expr(init, Use::Move);
                }
                self.declare(l);
            }
            (Some(l), Some(LocalKind::Owned { mutable: true })) => {
                // `var x = ...` owns its value: from a temporary, `take place`, or `.clone()`.
                if init.is_place() && root_local(init).is_some() && !self.is_copy(init.ty) {
                    let what = self.place(init).map(|p| self.describe(&p)).unwrap_or_default();
                    self.err(
                        Diagnostic::new(
                            codes::E0517,
                            init.span,
                            format!("`var` owns its value, so it can't share `{what}`"),
                        )
                        .with_help(format!(
                            "move it with `take {what}`, or copy it with `{what}.clone()`"
                        ))
                        .with_fix(
                            format!("copy it: `{what}.clone()`"),
                            init.span.shrink_to_end(),
                            ".clone()",
                        ),
                    );
                    self.expr(init, Use::Read);
                } else if let ExprKind::Closure(id) = init.kind {
                    self.declare(l);
                    self.closure(id, Some(l), init.span);
                    return;
                } else {
                    self.expr(init, Use::Move);
                }
                self.declare(l);
            }
            _ => {
                if let (Some(l), ExprKind::Closure(id)) = (single, &init.kind) {
                    self.declare(l);
                    self.closure(*id, Some(l), init.span);
                    return;
                }
                // Destructuring, or an owned binding: projections into a place borrow it.
                let proj: Vec<LocalId> = locals
                    .iter()
                    .copied()
                    .filter(|l| matches!(self.local(*l).kind, LocalKind::Projection { .. }))
                    .collect();
                if !proj.is_empty()
                    && let Some(place) = self.place(init).filter(|_| root_local(init).is_some())
                {
                    let until = proj.iter().map(|h| self.live_end[h.index()]).max().unwrap_or(0);
                    self.access(&place, Access::Borrow, init.span, proj.first().copied(), until);
                } else {
                    self.expr(init, Use::Move);
                }
                for l in locals {
                    self.declare(l);
                }
            }
        }
    }

    fn call_projection(&mut self, c: &Call, holder: LocalId, mutable: bool) {
        let until = self.live_end[holder.index()];
        let id = self.begin_call();
        for (a, &m) in c.args.iter().zip(&c.modes) {
            let inner = match &a.kind {
                ExprKind::MutArg(x) => x.as_ref(),
                _ => a,
            };
            match (m, self.place(inner).filter(|_| root_local(inner).is_some())) {
                (Mode::Borrow, Some(p)) => {
                    self.access(&p, Access::Borrow, a.span, Some(holder), until)
                }
                (Mode::Mut, Some(p)) => self.access(
                    &p,
                    if mutable { Access::MutBorrow } else { Access::Borrow },
                    a.span,
                    Some(holder),
                    until,
                ),
                (Mode::Take, _) | (_, None) => {
                    self.expr(a, if m == Mode::Take { Use::Move } else { Use::Read })
                }
            }
        }
        self.end_call(id);
    }

    /// A loop. Moves are checked twice: the second pass starts from what the first pass left,
    /// so a move of an outside value is caught where the next iteration uses it.
    fn loop_(&mut self, cond: Option<&Expr>, body: &Block, var: Option<LoopVar>) {
        let start = self.pos;
        let loans_before = self.loans.clone();
        let entry = self.moves.clone();
        self.loops.push(LoopFrame::default());
        let run = |w: &mut Self| {
            if let Some(c) = cond {
                w.expr(c, Use::Read);
            }
            if let Some((v, loan)) = &var {
                if let Some((p, mutable, span)) = loan {
                    let until = u32::MAX - 1;
                    w.access(
                        p,
                        if *mutable { Access::MutBorrow } else { Access::Borrow },
                        *span,
                        Some(*v),
                        until,
                    );
                }
                w.declare(*v);
            }
            w.block(body, Use::Read);
        };
        run(self);
        let end = self.pos;
        let frame = self.loops.pop().unwrap_or_default();
        // Liveness: a local from outside the loop used inside it lives to the loop's end.
        if self.liveness {
            let depth = self.loops.len() as u32;
            for l in frame.used {
                if self.decl_depth[l.index()] <= depth && self.live_end[l.index()] < end {
                    self.live_end[l.index()] = end;
                }
            }
        }
        // The loop variable's loan lasts the whole loop.
        for l in &mut self.loans {
            if l.until == u32::MAX - 1 {
                l.until = end;
            }
        }
        if !self.liveness {
            // Second pass for loop-carried moves.
            let after_first = merge(self.moves.clone(), frame.breaks.clone());
            let second_entry = merge(entry.clone(), self.moves.clone());
            if second_entry.as_ref().map(|m| m.len()) != entry.as_ref().map(|m| m.len()) {
                let saved_pos = self.pos;
                let saved_loans = self.loans.clone();
                let saved_diags = self.diags.len();
                self.pos = start;
                self.loans = loans_before;
                self.moves = second_entry;
                self.loops.push(LoopFrame::default());
                run(self);
                self.loops.pop();
                let new: Vec<Diagnostic> = self.diags.drain(saved_diags..).collect();
                for d in new {
                    if d.code == codes::E0500 || d.code == codes::E0516 {
                        // Rebuilt: the move happens in the loop, and the loop comes back to it.
                        let moved_at = d.secondary.first().map_or(d.primary.span, |l| l.span);
                        let what = d.message.split('`').nth(1).unwrap_or("it").to_string();
                        let mut e = Diagnostic::new(
                            codes::E0515,
                            d.primary.span,
                            format!(
                                "`{what}` is moved inside this loop, so the next iteration can't use it"
                            ),
                        );
                        if moved_at != d.primary.span {
                            e = e.with_secondary(moved_at, "moved here");
                        }
                        e = e
                            .with_note(
                                "a value from outside a loop can be moved inside it at most once",
                            )
                            .with_help(format!(
                                "move `{what}` before the loop, or move `{what}.clone()` inside it"
                            ));
                        self.diags.push(e);
                    }
                }
                self.pos = saved_pos;
                self.loans = saved_loans;
            }
            let exit = if cond.is_some() || var.is_some() {
                merge(entry, after_first)
            } else {
                frame.breaks
            };
            self.moves =
                if frame.has_break || cond.is_some() || var.is_some() { exit } else { None };
        }
    }

    // ---- projections out of the function ---------------------------------------------------

    /// `-> borrow T` and `-> mut T`: the returned place comes from a `borrow` or `mut`
    /// parameter (§6.4), and `-> mut` returns `mut place`.
    fn check_projection_return(&mut self, e: &Expr) {
        let Some(mode) = self.ret_mode else { return };
        if mode == RetMode::Owned || self.liveness {
            return;
        }
        match &e.kind {
            ExprKind::Block(b) => {
                if let Some(t) = &b.tail {
                    self.check_projection_return(t);
                }
                return;
            }
            ExprKind::If { then, else_, .. } => {
                if let Some(t) = &then.tail {
                    self.check_projection_return(t);
                }
                if let Some(x) = else_ {
                    self.check_projection_return(x);
                }
                return;
            }
            ExprKind::Match { arms, .. } => {
                for a in arms {
                    self.check_projection_return(&a.body);
                }
                return;
            }
            ExprKind::Return(_) | ExprKind::Error => return,
            _ => {}
        }
        let (inner, marked_mut) = match &e.kind {
            ExprKind::MutArg(x) => (x.as_ref(), true),
            _ => (e, false),
        };
        if mode == RetMode::Mut && !marked_mut {
            self.err(
                Diagnostic::new(
                    codes::E0503,
                    e.span,
                    "a `mut` projection is returned as `mut place`",
                )
                .with_fix("add `mut`", e.span.shrink_to_start(), "mut "),
            );
        }
        if let ExprKind::Call(c) = &inner.kind
            && c.ret_mode != RetMode::Owned
        {
            return; // a projection from another projection-returning call
        }
        match root_local(inner).filter(|_| inner.is_place()) {
            Some(l) => {
                let decl = self.local(l).clone();
                match decl.kind {
                    LocalKind::Param(Mode::Mut) => {}
                    LocalKind::Param(Mode::Borrow) if mode == RetMode::Borrow => {}
                    LocalKind::Param(Mode::Borrow) => self.err(
                        Diagnostic::new(codes::E0512, inner.span, format!("can't return a `mut` projection of `{}`: it's borrowed, not `mut`", decl.name))
                            .with_help(format!("make the parameter `{}: mut ...`", decl.name)),
                    ),
                    _ => self.err(
                        Diagnostic::new(codes::E0508, inner.span, format!("a projection must come from a `borrow` or `mut` parameter, and `{}` isn't one", decl.name))
                            .with_note("a projection can't outlive what it points into; this function's locals end when it returns (§6.4)")
                            .with_help("return an owned value instead: change the return type to `-> T` and return a copy or a new value"),
                    ),
                }
            }
            None => self.err(
                Diagnostic::new(codes::E0508, inner.span, "a projection must come from a `borrow` or `mut` parameter, and this is a temporary")
                    .with_help("return an owned value instead: `-> T`"),
            ),
        }
    }
}

/// The locals a pattern binds.
fn bindings(p: &Pat) -> Vec<LocalId> {
    let mut out = Vec::new();
    fn go(p: &Pat, out: &mut Vec<LocalId>) {
        match &p.kind {
            PatKind::Bind(l) => out.push(*l),
            PatKind::Tuple(ps) | PatKind::Or(ps) => ps.iter().for_each(|x| go(x, out)),
            PatKind::Adt { fields, .. } => fields.iter().for_each(|(_, x)| go(x, out)),
            _ => {}
        }
    }
    go(p, &mut out);
    out
}

fn innermost(e: &Expr) -> &Expr {
    match &e.kind {
        ExprKind::Field(b, _) | ExprKind::Swizzle(b, _) | ExprKind::Index(b, _) => innermost(b),
        _ => e,
    }
}
