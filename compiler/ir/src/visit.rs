//! Walking IR code: the statements of a block and its nested blocks (and rebuilding them), the
//! values an expression reads and the places it touches, the function a call calls, and the
//! functions a function reaches.
//!
//! Every match here names every variant, so a new statement or expression has to say what it
//! uses here, once, rather than in each pass that walks code.

use crate::*;

impl Place {
    /// The values the place reads: a pointer root, and each element index.
    pub fn for_each_value(&self, f: &mut impl FnMut(ValueId)) {
        if let PlaceRoot::Ptr(v) = self.root {
            f(v);
        }
        for p in &self.path {
            match p {
                Proj::Index(v) => f(*v),
                Proj::Field(_) | Proj::Comp(_) => {}
            }
        }
    }

    pub fn for_each_value_mut(&mut self, f: &mut impl FnMut(&mut ValueId)) {
        if let PlaceRoot::Ptr(v) = &mut self.root {
            f(v);
        }
        for p in &mut self.path {
            match p {
                Proj::Index(v) => f(v),
                Proj::Field(_) | Proj::Comp(_) => {}
            }
        }
    }

    /// The local the place is in, if it's in one.
    pub fn root_local(&self) -> Option<LocalId> {
        match self.root {
            PlaceRoot::Local(l) => Some(l),
            PlaceRoot::Param(_)
            | PlaceRoot::Resource(_)
            | PlaceRoot::Ptr(_)
            | PlaceRoot::Data(_) => None,
        }
    }

    /// Whether the place's path has an element index (which a value gives, and which is
    /// checked on the CPU).
    pub fn has_index(&self) -> bool {
        self.path.iter().any(|p| match p {
            Proj::Index(_) => true,
            Proj::Field(_) | Proj::Comp(_) => false,
        })
    }
}

impl Expr {
    /// Every value the expression reads, its places' included.
    pub fn for_each_value(&self, f: &mut impl FnMut(ValueId)) {
        match self {
            Expr::Const(_)
            | Expr::Zero(_)
            | Expr::EntryInput(_)
            | Expr::Param(_)
            | Expr::Barrier
            | Expr::Discard => {}
            Expr::Load(p) | Expr::Run(p) | Expr::Addr(p) | Expr::ArrayLength(p) => {
                p.for_each_value(f)
            }
            Expr::Atomic(_, p, xs) => {
                p.for_each_value(f);
                xs.iter().for_each(|x| f(*x));
            }
            Expr::Unary(_, x)
            | Expr::Extract(x, _)
            | Expr::Splat(x, _)
            | Expr::Swizzle(x, _)
            | Expr::Convert(x, _)
            | Expr::Bitcast(x, _)
            | Expr::Variant(_, _, Some(x)) => f(*x),
            Expr::Variant(_, _, None) => {}
            Expr::Binary(_, a, b) | Expr::ExtractDyn(a, b) => {
                f(*a);
                f(*b);
            }
            Expr::Builtin(_, xs)
            | Expr::Construct(_, xs)
            | Expr::Host(_, xs)
            | Expr::Mem(_, xs)
            | Expr::Texture(_, _, _, xs) => xs.iter().for_each(|x| f(*x)),
            Expr::Select { cond, if_true, if_false } => {
                f(*cond);
                f(*if_true);
                f(*if_false);
            }
            Expr::Call(_, args) => {
                for a in args {
                    match a {
                        Arg::Value(x) => f(*x),
                        Arg::Place(p) => p.for_each_value(f),
                    }
                }
            }
        }
    }

    pub fn for_each_value_mut(&mut self, f: &mut impl FnMut(&mut ValueId)) {
        match self {
            Expr::Const(_)
            | Expr::Zero(_)
            | Expr::EntryInput(_)
            | Expr::Param(_)
            | Expr::Barrier
            | Expr::Discard => {}
            Expr::Load(p) | Expr::Run(p) | Expr::Addr(p) | Expr::ArrayLength(p) => {
                p.for_each_value_mut(f)
            }
            Expr::Atomic(_, p, xs) => {
                p.for_each_value_mut(f);
                xs.iter_mut().for_each(f);
            }
            Expr::Unary(_, x)
            | Expr::Extract(x, _)
            | Expr::Splat(x, _)
            | Expr::Swizzle(x, _)
            | Expr::Convert(x, _)
            | Expr::Bitcast(x, _)
            | Expr::Variant(_, _, Some(x)) => f(x),
            Expr::Variant(_, _, None) => {}
            Expr::Binary(_, a, b) | Expr::ExtractDyn(a, b) => {
                f(a);
                f(b);
            }
            Expr::Builtin(_, xs)
            | Expr::Construct(_, xs)
            | Expr::Host(_, xs)
            | Expr::Mem(_, xs)
            | Expr::Texture(_, _, _, xs) => xs.iter_mut().for_each(f),
            Expr::Select { cond, if_true, if_false } => {
                f(cond);
                f(if_true);
                f(if_false);
            }
            Expr::Call(_, args) => {
                for a in args {
                    match a {
                        Arg::Value(x) => f(x),
                        Arg::Place(p) => p.for_each_value_mut(f),
                    }
                }
            }
        }
    }

    /// The places the expression reads or points to: a load's, a run's, an address's, a
    /// buffer's length, and a call's by-reference arguments.
    pub fn for_each_place(&self, f: &mut impl FnMut(&Place)) {
        match self {
            Expr::Load(p)
            | Expr::Run(p)
            | Expr::Addr(p)
            | Expr::ArrayLength(p)
            | Expr::Atomic(_, p, _) => f(p),
            Expr::Call(_, args) => {
                for a in args {
                    match a {
                        Arg::Place(p) => f(p),
                        Arg::Value(_) => {}
                    }
                }
            }
            Expr::Const(_)
            | Expr::Zero(_)
            | Expr::EntryInput(_)
            | Expr::Param(_)
            | Expr::Unary(..)
            | Expr::Extract(..)
            | Expr::Splat(..)
            | Expr::Swizzle(..)
            | Expr::Convert(..)
            | Expr::Bitcast(..)
            | Expr::Binary(..)
            | Expr::ExtractDyn(..)
            | Expr::Builtin(..)
            | Expr::Construct(..)
            | Expr::Variant(..)
            | Expr::Host(..)
            | Expr::Mem(..)
            | Expr::Texture(..)
            | Expr::Barrier
            | Expr::Discard
            | Expr::Select { .. } => {}
        }
    }

    pub fn for_each_place_mut(&mut self, f: &mut impl FnMut(&mut Place)) {
        match self {
            Expr::Load(p)
            | Expr::Run(p)
            | Expr::Addr(p)
            | Expr::ArrayLength(p)
            | Expr::Atomic(_, p, _) => f(p),
            Expr::Call(_, args) => {
                for a in args {
                    match a {
                        Arg::Place(p) => f(p),
                        Arg::Value(_) => {}
                    }
                }
            }
            Expr::Const(_)
            | Expr::Zero(_)
            | Expr::EntryInput(_)
            | Expr::Param(_)
            | Expr::Unary(..)
            | Expr::Extract(..)
            | Expr::Splat(..)
            | Expr::Swizzle(..)
            | Expr::Convert(..)
            | Expr::Bitcast(..)
            | Expr::Binary(..)
            | Expr::ExtractDyn(..)
            | Expr::Builtin(..)
            | Expr::Construct(..)
            | Expr::Variant(..)
            | Expr::Host(..)
            | Expr::Mem(..)
            | Expr::Texture(..)
            | Expr::Barrier
            | Expr::Discard
            | Expr::Select { .. } => {}
        }
    }

    /// Whether evaluating it does something besides give its value: a call, a host or memory
    /// operation, an atomic, a barrier. Those stay even when their value isn't used.
    pub fn has_effect(&self) -> bool {
        matches!(
            self,
            Expr::Call(..)
                | Expr::Host(..)
                | Expr::Mem(..)
                | Expr::Atomic(..)
                | Expr::Barrier
                | Expr::Discard
        )
    }

    /// Whether it only computes from its operands, reading no memory and doing nothing else:
    /// constants, arithmetic and math, and building vectors, matrices, structs and arrays or
    /// taking them apart. (An enum's variant isn't counted: the passes that ask leave it where
    /// it is.) A derivative counts, though it also wants the uniform control flow it's in.
    pub fn only_computes(&self) -> bool {
        match self {
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
            | Expr::Select { .. } => true,
            Expr::Load(_)
            | Expr::Call(..)
            | Expr::Variant(..)
            | Expr::Run(_)
            | Expr::Addr(_)
            | Expr::Host(..)
            | Expr::Mem(..)
            | Expr::ArrayLength(_)
            | Expr::Texture(..)
            | Expr::Atomic(..)
            | Expr::Barrier
            | Expr::Discard
            | Expr::EntryInput(_)
            | Expr::Param(_) => false,
        }
    }

    /// The function a call calls.
    pub fn callee(&self) -> Option<FuncId> {
        match self {
            Expr::Call(g, _) => Some(*g),
            _ => None,
        }
    }
}

impl Stmt {
    /// The expression the statement evaluates, if it evaluates one.
    pub fn expr(&self) -> Option<&Expr> {
        match self {
            Stmt::Let(_, e) | Stmt::Eval(e) => Some(e),
            Stmt::Store(..)
            | Stmt::If { .. }
            | Stmt::Loop { .. }
            | Stmt::Break
            | Stmt::Continue
            | Stmt::Return(_)
            | Stmt::Trap
            | Stmt::At(_) => None,
        }
    }

    /// The values the statement itself reads (not those of its nested blocks).
    pub fn for_each_value(&self, f: &mut impl FnMut(ValueId)) {
        match self {
            Stmt::Let(_, e) | Stmt::Eval(e) => e.for_each_value(f),
            Stmt::Store(p, v) => {
                p.for_each_value(f);
                f(*v);
            }
            Stmt::If { cond, .. } => f(*cond),
            Stmt::Return(Some(v)) => f(*v),
            Stmt::Loop { .. }
            | Stmt::Break
            | Stmt::Continue
            | Stmt::Return(None)
            | Stmt::Trap
            | Stmt::At(_) => {}
        }
    }

    pub fn for_each_value_mut(&mut self, f: &mut impl FnMut(&mut ValueId)) {
        match self {
            Stmt::Let(_, e) | Stmt::Eval(e) => e.for_each_value_mut(f),
            Stmt::Store(p, v) => {
                p.for_each_value_mut(f);
                f(v);
            }
            Stmt::If { cond, .. } => f(cond),
            Stmt::Return(Some(v)) => f(v),
            Stmt::Loop { .. }
            | Stmt::Break
            | Stmt::Continue
            | Stmt::Return(None)
            | Stmt::Trap
            | Stmt::At(_) => {}
        }
    }

    /// The places the statement itself touches: a store's target, and its expression's.
    pub fn for_each_place(&self, f: &mut impl FnMut(&Place)) {
        match self {
            Stmt::Let(_, e) | Stmt::Eval(e) => e.for_each_place(f),
            Stmt::Store(p, _) => f(p),
            Stmt::If { .. }
            | Stmt::Loop { .. }
            | Stmt::Break
            | Stmt::Continue
            | Stmt::Return(_)
            | Stmt::Trap
            | Stmt::At(_) => {}
        }
    }

    pub fn for_each_place_mut(&mut self, f: &mut impl FnMut(&mut Place)) {
        match self {
            Stmt::Let(_, e) | Stmt::Eval(e) => e.for_each_place_mut(f),
            Stmt::Store(p, _) => f(p),
            Stmt::If { .. }
            | Stmt::Loop { .. }
            | Stmt::Break
            | Stmt::Continue
            | Stmt::Return(_)
            | Stmt::Trap
            | Stmt::At(_) => {}
        }
    }

    /// The blocks nested in the statement: an `if`'s branches, a loop's body and continuing
    /// block, in that order.
    pub fn blocks(&self) -> impl DoubleEndedIterator<Item = &Block> {
        let (a, b) = match self {
            Stmt::If { then, else_, .. } => (Some(then), Some(else_)),
            Stmt::Loop { body, continuing } => (Some(body), Some(continuing)),
            Stmt::Let(..)
            | Stmt::Eval(_)
            | Stmt::Store(..)
            | Stmt::Break
            | Stmt::Continue
            | Stmt::Return(_)
            | Stmt::Trap
            | Stmt::At(_) => (None, None),
        };
        [a, b].into_iter().flatten()
    }

    pub fn blocks_mut(&mut self) -> impl DoubleEndedIterator<Item = &mut Block> {
        let (a, b) = match self {
            Stmt::If { then, else_, .. } => (Some(then), Some(else_)),
            Stmt::Loop { body, continuing } => (Some(body), Some(continuing)),
            Stmt::Let(..)
            | Stmt::Eval(_)
            | Stmt::Store(..)
            | Stmt::Break
            | Stmt::Continue
            | Stmt::Return(_)
            | Stmt::Trap
            | Stmt::At(_) => (None, None),
        };
        [a, b].into_iter().flatten()
    }
}

/// Calls `f` on every statement of `b` and of the blocks nested in it, each statement before
/// those nested in it.
pub fn walk(b: &[Stmt], f: &mut impl FnMut(&Stmt)) {
    for s in b {
        f(s);
        for inner in s.blocks() {
            walk(inner, f);
        }
    }
}

pub fn walk_mut(b: &mut Block, f: &mut impl FnMut(&mut Stmt)) {
    for s in b {
        f(s);
        for inner in s.blocks_mut() {
            walk_mut(inner, f);
        }
    }
}

/// Rebuilds `b` and the blocks nested in it: `f` is given each statement, after the blocks
/// nested in it are rebuilt, and appends what replaces it to the block being rebuilt.
pub fn expand(b: &mut Block, f: &mut impl FnMut(Stmt, &mut Block)) {
    for mut s in std::mem::take(b) {
        for inner in s.blocks_mut() {
            expand(inner, f);
        }
        f(s, b);
    }
}

/// [`expand`], with an `f` that can fail: the first error stops it.
pub fn try_expand(b: &mut Block, f: &mut impl FnMut(Stmt, &mut Block) -> Result<()>) -> Result<()> {
    for mut s in std::mem::take(b) {
        for inner in s.blocks_mut() {
            try_expand(inner, f)?;
        }
        f(s, b)?;
    }
    Ok(())
}

/// Whether `f` holds for a statement of `b` or of the blocks nested in it.
pub fn any(b: &[Stmt], f: &mut impl FnMut(&Stmt) -> bool) -> bool {
    b.iter().any(|s| f(s) || s.blocks().any(|inner| any(inner, f)))
}

/// Adds to `counts`, by local, how many places in `b` and the blocks nested in it mention it.
pub fn count_local_mentions(b: &Block, counts: &mut [u32]) {
    walk(b, &mut |s| {
        s.for_each_place(&mut |p| {
            if let Some(l) = p.root_local() {
                counts[l.index()] += 1;
            }
        })
    });
}

/// Every function a block calls, in order, with repeats.
pub fn calls(b: &Block) -> Vec<FuncId> {
    let mut out = Vec::new();
    walk(b, &mut |s| {
        if let Some(g) = s.expr().and_then(Expr::callee) {
            out.push(g);
        }
    });
    out
}

/// Every function `from` reaches through calls, `from` first, each once, in the order a
/// depth-first walk comes to them. The walk doesn't look inside `stop`'s functions: they're
/// reached, but not what they call.
pub fn reachable(m: &Module, from: FuncId, stop: &[FuncId]) -> Vec<FuncId> {
    let mut seen = vec![false; m.functions.len()];
    let mut stack = vec![from];
    let mut out = Vec::new();
    while let Some(f) = stack.pop() {
        if std::mem::replace(&mut seen[f.index()], true) {
            continue;
        }
        out.push(f);
        if !stop.contains(&f) {
            stack.extend(calls(&m.functions[f.index()].body));
        }
    }
    out
}
