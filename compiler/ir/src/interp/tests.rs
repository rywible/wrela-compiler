//! The interpreter over pure code (no resources): straight-line arithmetic, a loop with
//! `break` and `continue`, a `select`, locals, a struct and an array through a place, integer
//! wrap, and an index out of range.

use super::concrete::Given;
use super::*;

fn f32c(x: f32) -> Expr {
    Expr::Const(Const::F32(x))
}

fn u32c(x: u32) -> Expr {
    Expr::Const(Const::U32(x))
}

fn run(m: &Module, f: FuncId, args: Vec<V>) -> Outcome {
    let mut given = Given::default();
    Interp::new(m, &mut given, &mut ()).run_function(f, args).expect("runs")
}

fn result_f32(o: &Outcome) -> f32 {
    match &o.result {
        Some(V::S(Const::F32(x))) => *x,
        other => panic!("{other:?}"),
    }
}

#[test]
fn straight_line_arithmetic() {
    let mut m = Module::default();
    let t = m.types.f32();
    let mut f = Function::new("f", vec![Param::value("x", t)], Some(t));
    let mut b = Vec::new();
    let x = f.let_(&mut b, t, Expr::Param(0));
    let two = f.let_(&mut b, t, f32c(2.0));
    let y = f.let_(&mut b, t, Expr::Binary(BinOp::Mul, x, two));
    let s = f.let_(&mut b, t, Expr::Builtin(Builtin::Sqrt, vec![y]));
    let r = f.let_(&mut b, t, Expr::Binary(BinOp::Sub, s, x));
    b.push(Stmt::Return(Some(r)));
    f.body = b;
    let id = m.add_function(f);
    let o = run(&m, id, vec![V::S(Const::F32(8.0))]);
    assert_eq!(result_f32(&o), 4.0 - 8.0);
    assert!(o.undefined.is_empty());
    // `sqrt` of a negative is undefined in WGSL: computed as IEEE's NaN, and marked.
    let o = run(&m, id, vec![V::S(Const::F32(-8.0))]);
    assert!(result_f32(&o).is_nan());
    assert_eq!(o.undefined.len(), 2, "{:?}", o.undefined);
}

/// `var acc = 0; for i in 0..10 { if i == 3 { continue } if i == 6 { break } acc += i }`.
#[test]
fn a_loop_with_break_and_continue() {
    let mut m = Module::default();
    let u = m.types.u32();
    let bool_ = m.types.bool();
    let mut f = Function::new("f", Vec::new(), Some(u));
    let (acc, i) = (f.new_local("acc", u), f.new_local("i", u));
    let mut b = Vec::new();
    let zero = f.let_(&mut b, u, u32c(0));
    b.push(Stmt::Store(Place::local(acc), zero));
    b.push(Stmt::Store(Place::local(i), zero));
    let mut body = Vec::new();
    let iv = f.let_(&mut body, u, Expr::Load(Place::local(i)));
    let ten = f.let_(&mut body, u, u32c(10));
    let done = f.let_(&mut body, bool_, Expr::Binary(BinOp::Ge, iv, ten));
    body.push(Stmt::If { cond: done, then: vec![Stmt::Break], else_: Vec::new() });
    let three = f.let_(&mut body, u, u32c(3));
    let is3 = f.let_(&mut body, bool_, Expr::Binary(BinOp::Eq, iv, three));
    body.push(Stmt::If { cond: is3, then: vec![Stmt::Continue], else_: Vec::new() });
    let six = f.let_(&mut body, u, u32c(6));
    let is6 = f.let_(&mut body, bool_, Expr::Binary(BinOp::Eq, iv, six));
    body.push(Stmt::If { cond: is6, then: vec![Stmt::Break], else_: Vec::new() });
    let a = f.let_(&mut body, u, Expr::Load(Place::local(acc)));
    let a2 = f.let_(&mut body, u, Expr::Binary(BinOp::Add, a, iv));
    body.push(Stmt::Store(Place::local(acc), a2));
    let mut cont = Vec::new();
    let iv2 = f.let_(&mut cont, u, Expr::Load(Place::local(i)));
    let one = f.let_(&mut cont, u, u32c(1));
    let next = f.let_(&mut cont, u, Expr::Binary(BinOp::Add, iv2, one));
    cont.push(Stmt::Store(Place::local(i), next));
    b.push(Stmt::Loop { body, continuing: cont });
    let r = f.let_(&mut b, u, Expr::Load(Place::local(acc)));
    b.push(Stmt::Return(Some(r)));
    f.body = b;
    let id = m.add_function(f);
    let o = run(&m, id, Vec::new());
    assert_eq!(o.result, Some(V::S(Const::U32(1 + 2 + 4 + 5))));
}

#[test]
fn a_select_a_struct_and_an_array_through_places() {
    let mut m = Module::default();
    let (t, u, bool_) = (m.types.f32(), m.types.u32(), m.types.bool());
    let arr = m.types.intern(TypeDef::Array(t, 4));
    let st = m.types.intern(TypeDef::Struct {
        name: "S".into(),
        fields: vec![("a".into(), t), ("xs".into(), arr)],
    });
    let mut f = Function::new("f", vec![Param::value("k", u)], Some(t));
    let l = f.new_local("s", st);
    let mut b = Vec::new();
    let k = f.let_(&mut b, u, Expr::Param(0));
    let z = f.let_(&mut b, st, Expr::Zero(st));
    b.push(Stmt::Store(Place::local(l), z));
    let five = f.let_(&mut b, t, f32c(5.0));
    // s.xs[k] = 5; s.a = 1.5
    b.push(Stmt::Store(Place::local(l).with(Proj::Field(1)).with(Proj::Index(k)), five));
    let a = f.let_(&mut b, t, f32c(1.5));
    b.push(Stmt::Store(Place::local(l).with(Proj::Field(0)), a));
    // s.xs[2] + select(s.a, -s.a, k == 2)
    let two = f.let_(&mut b, u, u32c(2));
    let x =
        f.let_(&mut b, t, Expr::Load(Place::local(l).with(Proj::Field(1)).with(Proj::Index(two))));
    let av = f.let_(&mut b, t, Expr::Load(Place::local(l).with(Proj::Field(0))));
    let na = f.let_(&mut b, t, Expr::Unary(UnOp::Neg, av));
    let c = f.let_(&mut b, bool_, Expr::Binary(BinOp::Eq, k, two));
    let s = f.let_(&mut b, t, Expr::Select { cond: c, if_true: na, if_false: av });
    let r = f.let_(&mut b, t, Expr::Binary(BinOp::Add, x, s));
    b.push(Stmt::Return(Some(r)));
    f.body = b;
    let id = m.add_function(f);
    assert_eq!(result_f32(&run(&m, id, vec![V::S(Const::U32(2))])), 5.0 - 1.5);
    assert_eq!(result_f32(&run(&m, id, vec![V::S(Const::U32(1))])), 0.0 + 1.5);
    // An index out of range writes nothing, and is marked.
    let o = run(&m, id, vec![V::S(Const::U32(9))]);
    assert_eq!(result_f32(&o), 1.5);
    assert_eq!(o.out_of_range.len(), 1, "{:?}", o.out_of_range);
}

#[test]
fn integers_wrap_as_on_the_gpu() {
    let mut m = Module::default();
    let (u, i) = (m.types.u32(), m.types.scalar(Scalar::I32));
    let mut f = Function::new("f", vec![Param::value("x", u)], Some(i));
    let mut b = Vec::new();
    let x = f.let_(&mut b, u, Expr::Param(0));
    let big = f.let_(&mut b, u, u32c(u32::MAX));
    let s = f.let_(&mut b, u, Expr::Binary(BinOp::Add, x, big));
    let zero = f.let_(&mut b, u, u32c(0));
    // A division by zero gives the dividend (WGSL), marked.
    let d = f.let_(&mut b, u, Expr::Binary(BinOp::Div, s, zero));
    let c = f.let_(&mut b, i, Expr::Bitcast(d, Scalar::I32));
    b.push(Stmt::Return(Some(c)));
    f.body = b;
    let id = m.add_function(f);
    let o = run(&m, id, vec![V::S(Const::U32(3))]);
    assert_eq!(o.result, Some(V::S(Const::I32(2))));
    assert_eq!(o.undefined.len(), 1);
}

#[test]
fn an_observer_sees_and_may_change_values() {
    struct Double(u32);
    impl Observer for Double {
        fn value(&mut self, _f: FuncId, _v: ValueId, _ty: TypeId, val: &mut V) {
            if let V::S(Const::F32(x)) = val {
                self.0 += 1;
                *x *= 2.0;
            }
        }
    }
    let mut m = Module::default();
    let t = m.types.f32();
    let mut f = Function::new("f", Vec::new(), Some(t));
    let mut b = Vec::new();
    let one = f.let_(&mut b, t, f32c(1.0));
    let r = f.let_(&mut b, t, Expr::Binary(BinOp::Add, one, one));
    b.push(Stmt::Return(Some(r)));
    f.body = b;
    let id = m.add_function(f);
    let mut given = Given::default();
    let mut obs = Double(0);
    let o = Interp::new(&m, &mut given, &mut obs).run_function(id, Vec::new()).expect("runs");
    // 1 → 2, then 2 + 2 = 4 → 8.
    assert_eq!(result_f32(&o), 8.0);
    assert_eq!(obs.0, 2);
}
