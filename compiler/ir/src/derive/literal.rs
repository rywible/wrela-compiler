//! Parameter gradients (language.md §22): how a function's value changes with a lifted build's
//! literals, by the same forward-mode derivation as `gradient` (§13).
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
/// whose first field is its index in `table`), returning `ret`, `(f32, [f32; N])`: `f`'s value
/// and its derivative by each of the `N` literals. Without a table (a build that isn't lifted),
/// or where `f` reads none, every derivative is 0.
pub fn literal_gradient(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    table: Option<DataId>,
    which_ty: TypeId,
    ret: TypeId,
    target: Target,
) -> Result<FuncId> {
    let n = match m.types.get(which_ty) {
        TypeDef::Array(_, n) => *n,
        _ => return Err(Error::internal("a parameter gradient's literals aren't an array")),
    };
    if n > 255 {
        return Err(Error::not_derivable(
            "a parameter gradient takes at most 255 literals at once; ask in parts",
        ));
    }
    let func = m.functions[f.index()].clone();
    if func.ret != Some(m.types.f32()) || func.ret_ref {
        return Err(Error::not_derivable(
            "a parameter gradient needs a function that returns an `f32`",
        ));
    }
    let f32_t = m.types.f32();
    let grads_t = m.types.intern(TypeDef::Array(f32_t, n.max(1)));
    // What reads the table, and the copies that take it.
    let readers = match table {
        Some(t) => readers(m, f, t),
        None => HashMap::new(),
    };
    let mut w =
        Function::new(format!("{}_literal_gradient", func.name), func.params.clone(), Some(ret));
    w.params.push(Param { name: "which".into(), ty: which_ty, by_ref: false, mutable: false });
    let mut body = Vec::new();
    let args: Vec<Arg> = (0..func.params.len() as u32).map(|i| w.param_arg(i, &mut body)).collect();
    let zero = w.let_(&mut body, f32_t, Expr::Const(Const::F32(0.0)));
    let (value, grads) = match (table, readers.get(&f)) {
        (Some(table), Some(&copy)) if n > 0 => {
            let table_t = m.data[table.index()].ty;
            let mut mask = vec![0; func.params.len() + 1];
            mask[func.params.len()] = all_bits(&m.types, table_t);
            let d = ad::derive(m, cache, copy, mask.clone(), n as u8, target)?;
            let returns = returns_active(m, cache, copy, mask, Mode::Ad) != 0;
            // The seeds: the unit vector of each chosen literal.
            let which = w.let_(&mut body, which_ty, Expr::Param(func.params.len() as u32));
            let one = w.let_(&mut body, f32_t, Expr::Const(Const::F32(1.0)));
            let lit_t = match m.types.get(which_ty) {
                TypeDef::Array(e, _) => *e,
                _ => unreachable!("checked above"),
            };
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
                let v = w.let_(&mut body, f32_t, Expr::Extract(r, 0));
                let ds =
                    (0..n).map(|k| w.let_(&mut body, f32_t, Expr::Extract(r, 1 + k))).collect();
                (v, ds)
            } else {
                (r, vec![zero; n as usize])
            }
        }
        _ => {
            let v = w.let_(&mut body, f32_t, Expr::Call(f, args));
            (v, vec![zero; n as usize])
        }
    };
    let grads = if grads.is_empty() { vec![zero] } else { grads };
    let g = w.let_(&mut body, grads_t, Expr::Construct(grads_t, grads));
    let out = w.let_(&mut body, ret, Expr::Construct(ret, vec![value, g]));
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
