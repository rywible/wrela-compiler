//! Walking IR code: the statements of a block and its nested blocks, the values an expression
//! reads and the places it touches, and the function a call calls.
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
            PlaceRoot::Param(_) | PlaceRoot::Resource(_) | PlaceRoot::Ptr(_) => None,
        }
    }
}

impl Expr {
    /// Every value the expression reads, its places' included.
    pub fn for_each_value(&self, f: &mut impl FnMut(ValueId)) {
        match self {
            Expr::Const(_) | Expr::Zero(_) | Expr::EntryInput(_) | Expr::Param(_) => {}
            Expr::Load(p) | Expr::Run(p) | Expr::Addr(p) | Expr::ArrayLength(p) => {
                p.for_each_value(f)
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
            Expr::Builtin(_, xs) | Expr::Construct(_, xs) | Expr::Host(_, xs) => {
                xs.iter().for_each(|x| f(*x))
            }
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
            Expr::Const(_) | Expr::Zero(_) | Expr::EntryInput(_) | Expr::Param(_) => {}
            Expr::Load(p) | Expr::Run(p) | Expr::Addr(p) | Expr::ArrayLength(p) => {
                p.for_each_value_mut(f)
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
            Expr::Builtin(_, xs) | Expr::Construct(_, xs) | Expr::Host(_, xs) => {
                xs.iter_mut().for_each(f)
            }
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
            Expr::Load(p) | Expr::Run(p) | Expr::Addr(p) | Expr::ArrayLength(p) => f(p),
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
            | Expr::Select { .. } => {}
        }
    }

    pub fn for_each_place_mut(&mut self, f: &mut impl FnMut(&mut Place)) {
        match self {
            Expr::Load(p) | Expr::Run(p) | Expr::Addr(p) | Expr::ArrayLength(p) => f(p),
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
            | Expr::Select { .. } => {}
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

    pub fn expr_mut(&mut self) -> Option<&mut Expr> {
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
    /// block.
    pub fn blocks(&self) -> Vec<&Block> {
        match self {
            Stmt::If { then, else_, .. } => vec![then, else_],
            Stmt::Loop { body, continuing } => vec![body, continuing],
            Stmt::Let(..)
            | Stmt::Eval(_)
            | Stmt::Store(..)
            | Stmt::Break
            | Stmt::Continue
            | Stmt::Return(_)
            | Stmt::Trap
            | Stmt::At(_) => Vec::new(),
        }
    }

    pub fn blocks_mut(&mut self) -> Vec<&mut Block> {
        match self {
            Stmt::If { then, else_, .. } => vec![then, else_],
            Stmt::Loop { body, continuing } => vec![body, continuing],
            Stmt::Let(..)
            | Stmt::Eval(_)
            | Stmt::Store(..)
            | Stmt::Break
            | Stmt::Continue
            | Stmt::Return(_)
            | Stmt::Trap
            | Stmt::At(_) => Vec::new(),
        }
    }
}

/// Calls `f` on every statement of `b` and of the blocks nested in it, each statement before
/// those nested in it.
pub fn walk(b: &Block, f: &mut impl FnMut(&Stmt)) {
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
