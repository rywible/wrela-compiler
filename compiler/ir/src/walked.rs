//! Which bytes of a struct each loop walks (plan stage L.4, `wrela build --report layout`).
//!
//! For each loop, each struct whose elements it reaches one by one, through a container's
//! storage (a pointer to the struct made in the loop from `base + k * stride`, the struct's
//! stride, with `k` computed from a local the loop writes, its counter: a `Vec`'s or an
//! `Arena`'s elements, as indexing makes them) or an array or run of them indexed in it: the struct's size, the bytes of its fields the loop reads
//! and writes, and whether it ever takes an element whole as a place (loads or stores it whole,
//! lends it, or takes its address). What a loop nested in it reaches is that loop's. The rows
//! answer one question: where would keeping each field in its own array (columns) save the
//! bytes a pass skips over? Nothing here changes the module.

use crate::layout::{layout, parts};
use crate::{Arg, Block, Expr, Function, Module, Place, PlaceRoot, Proj, Stmt, TypeDef, TypeId};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use wrela_diag::Span;

/// One struct in one loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Walked {
    /// The function, and where its loop is (the last source location before it).
    pub function: String,
    pub at: Option<Span>,
    /// The struct's name and size in bytes.
    pub ty: String,
    pub size: u32,
    /// The bytes of the fields read, written, and either.
    pub read: u32,
    pub written: u32,
    pub touched: u32,
    /// An element taken whole as a place in the loop.
    pub whole: bool,
}

impl Walked {
    /// The bytes of each element a pass skips over.
    pub fn skipped(&self) -> u32 {
        self.size - self.touched
    }
}

/// Every loop's structs in `m`, the most bytes skipped first.
pub fn walked(m: &Module) -> Vec<Walked> {
    let mut out = Vec::new();
    for f in &m.functions {
        let mut defs = HashMap::new();
        crate::visit::walk(&f.body, &mut |s| {
            if let Stmt::Let(v, e) = s {
                defs.insert(*v, e);
            }
        });
        let mut cx = Cx { m, f, defs, written: Default::default(), out: &mut out };
        cx.block(&f.body);
    }
    out.sort_by(|a, b| {
        b.skipped().cmp(&a.skipped()).then(a.function.cmp(&b.function)).then(a.at.cmp(&b.at))
    });
    out
}

/// What one loop does with one struct: each field read and written, and whether it took one
/// whole.
#[derive(Default)]
struct Use {
    read: BTreeSet<u32>,
    written: BTreeSet<u32>,
    whole: bool,
}

struct Cx<'a> {
    m: &'a Module,
    f: &'a Function,
    /// Each value's definition.
    defs: HashMap<crate::ValueId, &'a Expr>,
    /// The locals the loop being scanned writes.
    written: std::collections::HashSet<crate::LocalId>,
    out: &'a mut Vec<Walked>,
}

impl Cx<'_> {
    /// A block outside any loop (or a loop's, done): each loop in it, with its location.
    fn block(&mut self, b: &Block) {
        let mut at = None;
        for s in b {
            match s {
                Stmt::At(sp) => at = Some(*sp),
                Stmt::Loop { body, continuing } => self.one_loop(at, &[body, continuing]),
                Stmt::If { then, else_, .. } => {
                    self.block(then);
                    self.block(else_);
                }
                _ => {}
            }
        }
    }

    fn one_loop(&mut self, at: Option<Span>, blocks: &[&Block]) {
        // The locals the loop writes (its counter among them).
        let mut written = std::collections::HashSet::new();
        for b in blocks {
            crate::visit::walk(b, &mut |s| {
                if let Stmt::Store(p, _) = s
                    && let PlaceRoot::Local(l) = p.root
                {
                    written.insert(l);
                }
            });
        }
        let saved = std::mem::replace(&mut self.written, written);
        // The pointers to a struct the loop makes, and what it does with each struct.
        let mut ptrs: HashMap<crate::ValueId, TypeId> = HashMap::new();
        let mut uses: BTreeMap<TypeId, Use> = BTreeMap::new();
        for b in blocks {
            self.scan(b, &mut ptrs, &mut uses);
        }
        for (t, u) in uses {
            let size = layout(&self.m.types, t).size;
            let fields = parts(&self.m.types, t);
            let bytes = |ks: &BTreeSet<u32>| -> u32 {
                ks.iter()
                    .filter_map(|k| fields.get(*k as usize))
                    .map(|(ft, _)| layout(&self.m.types, *ft).size)
                    .sum()
            };
            let touched: BTreeSet<u32> = u.read.union(&u.written).copied().collect();
            let (read, written, touched) = if u.whole {
                (size, size, size)
            } else {
                (bytes(&u.read), bytes(&u.written), bytes(&touched))
            };
            self.out.push(Walked {
                function: self.f.name.clone(),
                at,
                ty: self.m.types.display(t),
                size,
                read,
                written,
                touched,
                whole: u.whole,
            });
        }
        self.written = saved;
    }

    /// A loop's statements (a nested loop's are its own).
    fn scan(
        &mut self,
        b: &Block,
        ptrs: &mut HashMap<crate::ValueId, TypeId>,
        uses: &mut BTreeMap<TypeId, Use>,
    ) {
        let mut at = None;
        for s in b {
            match s {
                Stmt::At(sp) => at = Some(*sp),
                Stmt::Loop { body, continuing } => self.one_loop(at, &[body, continuing]),
                Stmt::If { then, else_, .. } => {
                    self.scan(then, ptrs, uses);
                    self.scan(else_, ptrs, uses);
                }
                Stmt::Let(v, Expr::Mem(crate::MemOp::Ptr, args)) => {
                    if let TypeDef::Ptr(t) = self.m.types.get(self.f.value_ty(*v))
                        && self.is_struct(*t)
                        && args.first().is_some_and(|a| self.strided(*a, *t))
                    {
                        ptrs.insert(*v, *t);
                    }
                }
                Stmt::Store(p, _) => self.place(p, true, false, ptrs, uses),
                _ => {}
            }
            if let Stmt::Let(_, e) | Stmt::Eval(e) = s {
                match e {
                    Expr::Load(p) => self.place(p, false, false, ptrs, uses),
                    Expr::Call(_, args) => {
                        for a in args {
                            if let Arg::Place(p) = a {
                                self.place(p, true, true, ptrs, uses);
                            }
                        }
                    }
                    Expr::Addr(p) | Expr::Run(p) => self.place(p, true, true, ptrs, uses),
                    Expr::Atomic(_, p, _) => self.place(p, true, false, ptrs, uses),
                    _ => {}
                }
            }
        }
    }

    /// Whether address `a` is `base + k * stride` (either way round), `stride` the stride of
    /// `t`'s elements: an element of a container's storage.
    fn strided(&self, a: crate::ValueId, t: TypeId) -> bool {
        let stride = layout(&self.m.types, t).stride();
        let is_const = |v: &crate::ValueId| matches!(self.defs.get(v), Some(Expr::Const(crate::Const::U32(n))) if *n == stride);
        let is_scaled = |v: &crate::ValueId| match self.defs.get(v) {
            Some(Expr::Binary(crate::BinOp::Mul | crate::BinOp::WrappingMul, x, y)) => {
                (is_const(x) && self.varies(*y, 16)) || (is_const(y) && self.varies(*x, 16))
            }
            _ => false,
        };
        match self.defs.get(&a) {
            Some(Expr::Binary(crate::BinOp::Add | crate::BinOp::WrappingAdd, x, y)) => {
                is_scaled(x) || is_scaled(y)
            }
            _ => false,
        }
    }

    /// Whether `v` is computed from a local the loop writes (looking `depth` definitions back).
    fn varies(&self, v: crate::ValueId, depth: u32) -> bool {
        if depth == 0 {
            return false;
        }
        match self.defs.get(&v) {
            Some(Expr::Load(p)) => {
                matches!(p.root, PlaceRoot::Local(l) if self.written.contains(&l))
            }
            Some(Expr::Binary(_, x, y)) => self.varies(*x, depth - 1) || self.varies(*y, depth - 1),
            Some(Expr::Unary(_, x) | Expr::Convert(x, _) | Expr::Bitcast(x, _)) => {
                self.varies(*x, depth - 1)
            }
            _ => false,
        }
    }

    fn is_struct(&self, t: TypeId) -> bool {
        matches!(self.m.types.get(t), TypeDef::Struct { .. })
    }

    /// A place the loop reads (`write`: or writes; `lent`: takes whole, whatever its path) of a
    /// struct element: through a pointer the loop made, or an array or run indexed.
    fn place(
        &self,
        p: &Place,
        write: bool,
        lent: bool,
        ptrs: &HashMap<crate::ValueId, TypeId>,
        uses: &mut BTreeMap<TypeId, Use>,
    ) {
        let (t, rest) = match &p.root {
            PlaceRoot::Ptr(v) if ptrs.contains_key(v) => (ptrs[v], p.path.as_slice()),
            _ => {
                // `xs[i]...`: the first index into an array or run of structs.
                let mut t = self.m.place_ty(self.f, &Place::root(p.root.clone()));
                let mut found = None;
                for (k, proj) in p.path.iter().enumerate() {
                    let Some(ty) = t else { break };
                    if let Proj::Index(ix) = proj
                        && let TypeDef::Run(e) | TypeDef::Array(e, _) = self.m.types.get(ty)
                        && self.is_struct(*e)
                        && self.varies(*ix, 16)
                    {
                        found = Some((*e, &p.path[k + 1..]));
                        break;
                    }
                    t = self.m.proj_ty(ty, proj);
                }
                let Some(f) = found else { return };
                f
            }
        };
        let u = uses.entry(t).or_default();
        match rest.first() {
            Some(Proj::Field(k)) if !lent => {
                if write {
                    u.written.insert(*k);
                } else {
                    u.read.insert(*k);
                }
            }
            _ => u.whole = true,
        }
    }
}
