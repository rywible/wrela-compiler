//! Debug builds' bounds checks on the GPU (language.md §11). WGSL doesn't trap: an index out of
//! range reads or writes some element in range, or a zero. In a debug build each dynamic index
//! in a pipeline's code is checked against its array's length first, and one out of range
//! records the pipeline's number (from 1) in a flag the host reads after each frame. The access
//! then goes on as WGSL defines it.
//!
//! Vertex shaders can't write storage, so code a vertex entry point reaches isn't checked.

use crate::*;
use std::collections::HashSet;

/// Adds the checks to a flattened GPU module, with the flag (`array<atomic<u32>>`) at
/// `binding`, set to `code` (at least it: the flag keeps the largest). `None` if the module
/// indexes nothing it checks, so it needs no flag.
pub fn add_checks(m: &mut Module, binding: u32, code: u32) -> Option<ResourceId> {
    let skip = vertex_reach(m);
    let any = (0..m.functions.len()).any(|i| {
        let mut found = false;
        visit::walk(&m.functions[i].body, &mut |s| {
            s.for_each_place(&mut |p| found |= p.has_index());
            found |= matches!(s.expr(), Some(Expr::ExtractDyn(..)));
        });
        found && !skip.contains(&FuncId(i as u32))
    });
    if !any {
        return None;
    }
    let u32_ty = m.types.u32();
    let flag_ty = m.types.intern(TypeDef::Atomic(Scalar::U32));
    let flag = m.add_resource(Resource {
        name: "wrela_debug_flag".into(),
        binding,
        kind: ResourceKind::StorageReadWrite,
        ty: flag_ty,
    });
    for i in 0..m.functions.len() {
        if skip.contains(&FuncId(i as u32)) {
            continue;
        }
        let mut f = std::mem::take(&mut m.functions[i]);
        let mut body = std::mem::take(&mut f.body);
        let mut cx = Checks { m, f: &mut f, flag, code, u32_ty };
        visit::expand(&mut body, &mut |s, out| cx.stmt(s, out));
        f.body = body;
        m.functions[i] = f;
    }
    Some(flag)
}

/// The functions a vertex entry point reaches.
fn vertex_reach(m: &Module) -> HashSet<FuncId> {
    let mut seen = HashSet::new();
    let mut todo: Vec<FuncId> = m
        .entry_points
        .iter()
        .filter(|e| matches!(e.stage, Stage::Vertex { .. }))
        .map(|e| e.function)
        .collect();
    while let Some(f) = todo.pop() {
        if seen.insert(f) {
            todo.extend(visit::calls(&m.functions[f.index()].body));
        }
    }
    seen
}

struct Checks<'a> {
    m: &'a mut Module,
    f: &'a mut Function,
    flag: ResourceId,
    code: u32,
    u32_ty: TypeId,
}

/// How long an indexed array is.
enum Len {
    Const(u32),
    /// A storage buffer's.
    Of(PlaceRoot),
}

impl Checks<'_> {
    /// `s`, after a check of each index it takes.
    fn stmt(&mut self, s: Stmt, out: &mut Block) {
        let mut checks = Vec::new();
        s.for_each_place(&mut |p| self.place_checks(p, &mut checks));
        if let Some(Expr::ExtractDyn(x, i)) = s.expr() {
            let n = match self.m.types.get(self.f.value_ty(*x)) {
                TypeDef::Array(_, n) => Some(*n),
                TypeDef::Vector(_, n) | TypeDef::Matrix(n) => Some(u32::from(*n)),
                _ => None,
            };
            if let Some(n) = n {
                checks.push((*i, Len::Const(n)));
            }
        }
        for (i, len) in checks {
            self.check(out, i, len);
        }
        out.push(s);
    }

    /// The indices a place takes, each with its array's length.
    fn place_checks(&self, p: &Place, out: &mut Vec<(ValueId, Len)>) {
        let storage = matches!(&p.root, PlaceRoot::Resource(r)
            if matches!(self.m.resources[r.index()].kind,
                ResourceKind::StorageRead | ResourceKind::StorageReadWrite));
        if storage && let Some(Proj::Index(i)) = p.path.first() {
            // The flag's own index (a zero) isn't checked.
            if !matches!(p.root, PlaceRoot::Resource(r) if r == self.flag) {
                out.push((*i, Len::Of(p.root.clone())));
            }
        }
        let Some((mut t, path)) = self.m.place_start(self.f, p) else { return };
        for proj in path {
            if let Proj::Index(i) = proj {
                match self.m.types.get(t) {
                    TypeDef::Array(_, n) => out.push((*i, Len::Const(*n))),
                    TypeDef::Vector(_, n) | TypeDef::Matrix(n) => {
                        out.push((*i, Len::Const(u32::from(*n))))
                    }
                    _ => {}
                }
            }
            let Some(next) = self.m.proj_ty(t, proj) else { return };
            t = next;
        }
    }

    /// `if i >= len { atomicMax(&flag[0], code) }`, with `i` as a `u32` (a negative `i32`
    /// is then out of range too).
    fn check(&mut self, out: &mut Block, i: ValueId, len: Len) {
        let u = self.u32_ty;
        let i = match self.m.types.get(self.f.value_ty(i)) {
            TypeDef::Scalar(Scalar::U32) => i,
            TypeDef::Scalar(Scalar::I32) => self.f.let_(out, u, Expr::Bitcast(i, Scalar::U32)),
            _ => self.f.let_(out, u, Expr::Convert(i, Scalar::U32)),
        };
        let len = match len {
            Len::Const(n) => self.f.let_(out, u, Expr::Const(Const::U32(n))),
            Len::Of(root) => self.f.let_(out, u, Expr::ArrayLength(Place::root(root))),
        };
        let b = self.m.types.bool();
        let bad = self.f.let_(out, b, Expr::Binary(BinOp::Ge, i, len));
        let mut then = Vec::new();
        let zero = self.f.let_(&mut then, u, Expr::Const(Const::U32(0)));
        let code = self.f.let_(&mut then, u, Expr::Const(Const::U32(self.code)));
        let at = Place { root: PlaceRoot::Resource(self.flag), path: vec![Proj::Index(zero)] };
        then.push(Stmt::Eval(Expr::Atomic(AtomicOp::Max, at, vec![code])));
        out.push(Stmt::If { cond: bad, then, else_: Vec::new() });
    }
}
