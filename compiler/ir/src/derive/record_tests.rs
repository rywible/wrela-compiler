//! Reverse mode's slots (`ad::Ad::record`): a value's place on the trace travels as its bits,
//! moved by the same instructions that move the value.

use super::*;
use crate::interp::concrete::Given;
use crate::interp::{Interp, V};

/// `f(x) = h(select(true, vec3(x.y, x.z, x.x).zxy, 0)).y` with `h` the identity: only moves,
/// so the slot it returns is the slot of the input number it returns, bit for bit.
#[test]
fn a_record_moves_slots_bit_for_bit() {
    let mut m = Module::default();
    let (t, v3, u, b) = (m.types.f32(), m.types.vector(3), m.types.u32(), m.types.bool());
    let trace =
        m.types.intern(TypeDef::Struct { name: "Trace".into(), fields: vec![("n".into(), u)] });
    // The trace's pushes: never called for moves alone.
    let push = |m: &mut Module, n: usize| {
        let mut params = vec![Param { name: "t".into(), ty: trace, by_ref: true, mutable: true }];
        for _ in 0..n {
            params.push(Param::value("a", u));
            params.push(Param::value("d", t));
        }
        let mut f = Function::new("push", params, Some(u));
        f.body.push(Stmt::Trap);
        m.add_function(f)
    };
    let (one, two) = (push(&mut m, 1), push(&mut m, 2));
    let mut h = Function::new("h", vec![Param::value("v", v3)], Some(v3));
    let mut hb = Vec::new();
    let hv = h.let_(&mut hb, v3, Expr::Param(0));
    hb.push(Stmt::Return(Some(hv)));
    h.body = hb;
    let h = m.add_function(h);
    let mut f = Function::new("f", vec![Param::value("x", v3)], Some(t));
    let mut fb = Vec::new();
    let x = f.let_(&mut fb, v3, Expr::Param(0));
    let (xx, xy, xz) = (
        f.let_(&mut fb, t, Expr::Extract(x, 0)),
        f.let_(&mut fb, t, Expr::Extract(x, 1)),
        f.let_(&mut fb, t, Expr::Extract(x, 2)),
    );
    let built = f.let_(&mut fb, v3, Expr::Construct(v3, vec![xy, xz, xx]));
    let turned = f.let_(&mut fb, v3, Expr::Swizzle(built, vec![2, 0, 1]));
    let zero = f.let_(&mut fb, v3, Expr::Zero(v3));
    let yes = f.let_(&mut fb, b, Expr::Const(Const::Bool(true)));
    let chosen = f.let_(&mut fb, v3, Expr::Select { cond: yes, if_true: turned, if_false: zero });
    let passed = f.let_(&mut fb, v3, Expr::Call(h, vec![Arg::Value(chosen)]));
    let r = f.let_(&mut fb, t, Expr::Extract(passed, 1));
    fb.push(Stmt::Return(Some(r)));
    f.body = fb;
    let f = m.add_function(f);
    let fns = ad::RecordFns { one, two, trace, leaf: None };
    let mut cache = DeriveCache::default();
    let mask = vec![all_bits(&m.types, v3)];
    let d = ad::derive(&mut m, &mut cache, f, mask, 1, Target::Cpu, false, Some(fns))
        .expect("recorded");
    // A caller with a trace of its own.
    let dual = m.functions[d.index()].ret.expect("a dual");
    let mut g = Function::new("g", vec![Param::value("x", v3), Param::value("s", v3)], Some(dual));
    let tl = g.new_local("trace", trace);
    let mut gb = Vec::new();
    let (gx, gs) = (g.let_(&mut gb, v3, Expr::Param(0)), g.let_(&mut gb, v3, Expr::Param(1)));
    let out = g.let_(
        &mut gb,
        dual,
        Expr::Call(d, vec![Arg::Value(gx), Arg::Value(gs), Arg::Place(Place::local(tl))]),
    );
    gb.push(Stmt::Return(Some(out)));
    g.body = gb;
    let g = m.add_function(g);
    crate::verify(&m).expect("the module verifies");
    // Slots 7, 8 and 9 for x, y and z, as their bits: the result is `x.y`'s value... after the
    // turn, (x.y, x.z, x.x).zxy = (x.x, x.y, x.z), whose `.y` is `x.y`, slot 8.
    let fv = |xs: [f32; 3]| V::P(xs.iter().map(|x| V::S(Const::F32(*x))).collect());
    let slots = [7u32, 8, 9].map(f32::from_bits);
    let mut given = Given::default();
    let o = Interp::new(&m, &mut given, &mut ())
        .run_function(g, vec![fv([1.5, 2.5, 3.5]), fv(slots)])
        .expect("runs");
    let Some(V::P(parts)) = o.result else { panic!("{:?}", o.result) };
    assert_eq!(parts[0], V::S(Const::F32(2.5)));
    let V::S(Const::F32(slot)) = parts[1] else { panic!("{parts:?}") };
    assert_eq!(slot.to_bits(), 8);
}
