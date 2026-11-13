//! Parameter gradients (language.md §23): how a function's value changes with a lifted build's
//! literals, by the same forward-mode derivation as `gradient` (§14). The value is an `f32`
//! (`std::lift::gradient`) or any value a derivation carries tangents of, such as an array of
//! `f32` (`std::lift::jacobian`): each literal's derivative is a value of the same type.
//!
//! A lifted literal is a read of the program's table (`Load(table[i])`), deep in whatever code
//! reads it. The derivation makes the table an input: each function the derived one reaches
//! that reads the table, directly or through its calls, is copied with the table as a parameter
//! taken by reference, and its reads read the parameter. Deriving the copy by that parameter, in
//! `n` directions, gives the derivative by every literal at once in each direction; the
//! direction for literal `i` is the table's unit vector `e_i`. So `which` chooses the
//! directions, and the code that builds values from literals (a creature's skeleton) is derived
//! as well as the code that reads them.

use super::*;

/// A function of `f`'s parameters (its captures) and `which` (`[Literal; N]`, each a struct
/// whose first field is its index in `table`), returning `ret`, `(R, [R; N])` for `f`'s result
/// type `R`: `f`'s value and its derivative by each of the `N` literals. Without a table (a
/// build that isn't lifted), or where `f` reads none, every derivative is 0.
pub fn literal_gradient(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    table: Option<DataId>,
    which_ty: TypeId,
    ret: TypeId,
    target: Target,
) -> Result<FuncId> {
    let (lit_t, n) = match m.types.get(which_ty) {
        TypeDef::Array(e, n) => (*e, *n),
        _ => return Err(Error::internal("a parameter gradient's literals aren't an array")),
    };
    if n > 255 {
        return Err(Error::not_derivable(
            "a parameter gradient takes at most 255 literals at once; ask in parts",
        ));
    }
    let func = m.functions[f.index()].clone_signature();
    let value_t = match func.ret {
        Some(r) if !func.ret_ref => r,
        _ => {
            return Err(Error::not_derivable(
                "a parameter gradient needs a function that returns a value",
            ));
        }
    };
    let f32_t = m.types.f32();
    let grads_t = m.types.intern(TypeDef::Array(value_t, n));
    // What reads the table, and the copies that take it.
    let readers = match table {
        Some(t) => readers(m, f, t),
        None => HashMap::new(),
    };
    let mut w =
        Function::new(format!("{}_literal_gradient", func.name), func.params.clone(), Some(ret));
    w.params.push(Param::value("which", which_ty));
    let mut body = Vec::new();
    let args: Vec<Arg> = (0..func.params.len() as u32).map(|i| w.param_arg(i, &mut body)).collect();
    let zero = w.let_(&mut body, value_t, Expr::Zero(value_t));
    let (value, grads) = match (table, readers.get(&f)) {
        (Some(table), Some(&copy)) if n > 0 => {
            let table_t = m.data[table.index()].ty;
            let mut mask = vec![0; func.params.len() + 1];
            mask[func.params.len()] = all_bits(&m.types, table_t);
            let d = ad::derive(m, cache, copy, mask.clone(), n as u8, target, false, None)?;
            let returns = returns_active(m, cache, copy, mask, Mode::Ad) != 0;
            // The seeds: the unit vector of each chosen literal.
            let which = w.let_(&mut body, which_ty, Expr::Param(func.params.len() as u32));
            let one = w.let_(&mut body, f32_t, Expr::Const(Const::F32(1.0)));
            let u = m.types.u32();
            let mut args = args;
            args.push(Arg::Place(Place::root(PlaceRoot::Data(table))));
            for k in 0..n {
                let seed = w.new_local(format!("seed{k}"), table_t);
                let z = w.let_(&mut body, table_t, Expr::Zero(table_t));
                body.push(Stmt::Store(Place::local(seed), z));
                let lit = w.let_(&mut body, lit_t, Expr::Extract(which, k));
                let i = w.let_(&mut body, u, Expr::Extract(lit, 0));
                body.push(Stmt::Store(Place::local(seed).with(Proj::Index(i)), one));
                args.push(Arg::Place(Place::local(seed)));
            }
            let dual = m.functions[d.index()]
                .ret
                .ok_or_else(|| Error::internal("a derived function returns nothing"))?;
            let r = w.let_(&mut body, dual, Expr::Call(d, args));
            if returns {
                let v = w.let_(&mut body, value_t, Expr::Extract(r, 0));
                let ds =
                    (0..n).map(|k| w.let_(&mut body, value_t, Expr::Extract(r, 1 + k))).collect();
                (v, ds)
            } else {
                (r, vec![zero; n as usize])
            }
        }
        _ => {
            let v = w.let_(&mut body, value_t, Expr::Call(f, args));
            (v, vec![zero; n as usize])
        }
    };
    let g = w.let_(&mut body, grads_t, Expr::Construct(grads_t, grads));
    let out = w.let_(&mut body, ret, Expr::Construct(ret, vec![value, g]));
    body.push(Stmt::Return(Some(out)));
    w.body = body;
    Ok(m.add_function(w))
}

/// Reverse mode for lifted literals (`std::lift::recorded`): a function of `f`'s parameters
/// (its captures), `which` (`[Literal; N]`) and the trace (by reference, last), returning `ret`,
/// `(f32, f32)`: `f`'s value and its node on the trace. The literals of `which` are the trace's
/// next `N` nodes, in order (none are pushed where `f` reads no literal: its node is 0; a
/// literal named twice is derived at its first node, its later ones' derivatives 0): the table-taking copies of what reads the table ([`readers`]) are
/// derived recording, the shadow table zero but at each chosen literal's index, which holds its
/// node's bits. No limit of 255: the cost is one recorded evaluation however many literals.
#[allow(clippy::too_many_arguments)]
pub fn literal_recorded(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    table: Option<DataId>,
    which_ty: TypeId,
    fns: ad::RecordFns,
    ret: TypeId,
) -> Result<FuncId> {
    let (lit_t, n) = match m.types.get(which_ty) {
        TypeDef::Array(e, n) => (*e, *n),
        _ => return Err(Error::internal("a reverse gradient's literals aren't an array")),
    };
    let func = m.functions[f.index()].clone_signature();
    let f32_t = m.types.f32();
    if func.ret != Some(f32_t) || func.ret_ref {
        return Err(Error::not_derivable(
            "a reverse gradient needs a function that returns an `f32`",
        ));
    }
    let leaf = fns.leaf.ok_or_else(|| Error::internal("no `trace_leaf`"))?;
    let readers = match table {
        Some(t) => readers(m, f, t),
        None => HashMap::new(),
    };
    let mut params = func.params.clone();
    let wi = params.len() as u32;
    params.push(Param::value("which", which_ty));
    params.push(Param { name: "trace".into(), ty: fns.trace, by_ref: true, mutable: true });
    let trace = || Arg::Place(Place::root(PlaceRoot::Param(wi + 1)));
    let mut w = Function::new(format!("{}_literals_recorded", func.name), params, Some(ret));
    let mut body = Vec::new();
    let mut args: Vec<Arg> =
        (0..func.params.len() as u32).map(|i| w.param_arg(i, &mut body)).collect();
    let (u, b) = (m.types.u32(), m.types.bool());
    let (value, slot) = match (table, readers.get(&f)) {
        (Some(table), Some(&copy)) if n > 0 => {
            let table_t = m.data[table.index()].ty;
            let mut mask = vec![0; func.params.len() + 1];
            mask[func.params.len()] = all_bits(&m.types, table_t);
            let d = ad::derive(m, cache, copy, mask.clone(), 1, Target::Cpu, false, Some(fns))?;
            let returns = returns_active(m, cache, copy, mask, Mode::Ad) != 0;
            // The shadow table: zero, but each chosen literal's node at its index.
            let shadow = w.new_local("slots", table_t);
            let z = w.let_(&mut body, table_t, Expr::Zero(table_t));
            body.push(Stmt::Store(Place::local(shadow), z));
            let which = w.new_local("which", which_ty);
            let wv = w.let_(&mut body, which_ty, Expr::Param(wi));
            body.push(Stmt::Store(Place::local(which), wv));
            let k = w.new_local("k", u);
            let zero = w.let_(&mut body, u, Expr::Const(Const::U32(0)));
            body.push(Stmt::Store(Place::local(k), zero));
            let mut lb = Vec::new();
            let kv = w.let_(&mut lb, u, Expr::Load(Place::local(k)));
            let end = w.let_(&mut lb, u, Expr::Const(Const::U32(n)));
            let done = w.let_(&mut lb, b, Expr::Binary(BinOp::Ge, kv, end));
            lb.push(Stmt::If { cond: done, then: vec![Stmt::Break], else_: Vec::new() });
            let lit = w.let_(&mut lb, lit_t, Expr::Load(Place::local(which).with(Proj::Index(kv))));
            let i = w.let_(&mut lb, u, Expr::Extract(lit, 0));
            let node = w.let_(&mut lb, u, Expr::Call(leaf, vec![trace()]));
            let bits = w.let_(&mut lb, f32_t, Expr::Bitcast(node, Scalar::F32));
            // A literal named twice keeps its first node: the later ones' derivatives are 0.
            let had = w.let_(&mut lb, f32_t, Expr::Load(Place::local(shadow).with(Proj::Index(i))));
            let had = w.let_(&mut lb, u, Expr::Bitcast(had, Scalar::U32));
            let first = w.let_(&mut lb, b, Expr::Binary(BinOp::Eq, had, zero));
            let store = vec![Stmt::Store(Place::local(shadow).with(Proj::Index(i)), bits)];
            lb.push(Stmt::If { cond: first, then: store, else_: Vec::new() });
            let mut cont = Vec::new();
            let kv2 = w.let_(&mut cont, u, Expr::Load(Place::local(k)));
            let one = w.let_(&mut cont, u, Expr::Const(Const::U32(1)));
            let next = w.let_(&mut cont, u, Expr::Binary(BinOp::WrappingAdd, kv2, one));
            cont.push(Stmt::Store(Place::local(k), next));
            body.push(Stmt::Loop { body: lb, continuing: cont });
            args.push(Arg::Place(Place::root(PlaceRoot::Data(table))));
            if returns {
                args.push(Arg::Place(Place::local(shadow)));
                args.push(trace());
                let dual = m.functions[d.index()]
                    .ret
                    .ok_or_else(|| Error::internal("a derived function returns nothing"))?;
                let r = w.let_(&mut body, dual, Expr::Call(d, args));
                let v = w.let_(&mut body, f32_t, Expr::Extract(r, 0));
                let s = w.let_(&mut body, f32_t, Expr::Extract(r, 1));
                (v, s)
            } else {
                let v = w.let_(&mut body, f32_t, Expr::Call(copy, args));
                (v, w.let_(&mut body, f32_t, Expr::Const(Const::F32(0.0))))
            }
        }
        _ => {
            // Nothing read: no node is pushed, and the value's node is 0.
            let v = w.let_(&mut body, f32_t, Expr::Call(f, args));
            (v, w.let_(&mut body, f32_t, Expr::Const(Const::F32(0.0))))
        }
    };
    let out = w.let_(&mut body, ret, Expr::Construct(ret, vec![value, slot]));
    body.push(Stmt::Return(Some(out)));
    w.body = body;
    Ok(m.add_function(w))
}

/// The functions `f` reaches (itself included) that read `table`, directly or through what
/// they call, each mapped to a copy that takes the table by reference as its last parameter.
fn readers(m: &mut Module, f: FuncId, table: DataId) -> HashMap<FuncId, FuncId> {
    let reached = visit::reachable(m, f, &[]);
    // Which of those read the table, through calls too: a fixpoint.
    let reads_directly = |func: &Function| {
        let mut yes = false;
        visit::walk(&func.body, &mut |s| {
            s.for_each_place(&mut |p| yes |= p.root == PlaceRoot::Data(table));
        });
        yes
    };
    let mut reads: HashMap<FuncId, bool> =
        reached.iter().map(|&g| (g, reads_directly(&m.functions[g.index()]))).collect();
    loop {
        let mut changed = false;
        for &g in &reached {
            if reads[&g] {
                continue;
            }
            if visit::calls(&m.functions[g.index()].body)
                .iter()
                .any(|c| reads.get(c) == Some(&true))
            {
                reads.insert(g, true);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let table_t = m.data[table.index()].ty;
    // The copies: declared first, so calls between them can name each other.
    let mut copies = HashMap::new();
    for &g in reached.iter().filter(|g| reads[g]) {
        let mut c = m.functions[g.index()].clone();
        c.name = format!("{}_lits", c.name);
        c.params.push(Param { name: "lits".into(), ty: table_t, by_ref: true, mutable: false });
        c.interval = false;
        copies.insert(g, m.add_function(c));
    }
    for (&g, &c) in &copies {
        let k = m.functions[g.index()].params.len() as u32;
        let func = &mut m.functions[c.index()];
        visit::walk_mut(&mut func.body, &mut |s| {
            s.for_each_place_mut(&mut |p| {
                if p.root == PlaceRoot::Data(table) {
                    p.root = PlaceRoot::Param(k);
                }
            });
            let call = match s {
                Stmt::Let(_, Expr::Call(h, args)) | Stmt::Eval(Expr::Call(h, args)) => {
                    Some((h, args))
                }
                _ => None,
            };
            if let Some((h, args)) = call
                && let Some(&hc) = copies.get(&*h)
            {
                *h = hc;
                args.push(Arg::Place(Place::root(PlaceRoot::Param(k))));
            }
        });
    }
    copies
}
