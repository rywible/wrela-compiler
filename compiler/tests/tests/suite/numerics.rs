//! AC7: strict CPU numerics, run under wasmtime. Integer arithmetic that overflows, divides by
//! zero or shifts by the type's width or more traps; wrapping arithmetic and integer-to-integer
//! conversions keep the low bits; float-to-integer conversions truncate, and trap out of range
//! or on NaN; an index out of range traps; float `%` is exact (C's `fmod`, which Rust's `%`
//! is); an exported function's 8- and 16-bit and `bool` parameters are brought into range. (The state hash agreeing between Chrome and wasmtime
//! is tests/suite/hello_field.rs; the module check for relaxed SIMD is wrela-wasm's.)

use crate::built;
use wrela_host::{CpuHost, Error, Value};
use wrela_tests::Rng;

fn load() -> CpuHost {
    CpuHost::load(built("numerics")).expect("load")
}

fn traps(host: &mut CpuHost, name: &str, args: &[Value]) {
    match host.call_export(name, args) {
        Err(Error::Trap(_)) => {}
        other => panic!("{name}{args:?} should trap, got {other:?}"),
    }
}

fn returns(host: &mut CpuHost, name: &str, args: &[Value], want: Value) {
    match host.call_export(name, args) {
        Ok(v) if v == [want] => {}
        other => panic!("{name}{args:?} should return {want:?}, got {other:?}"),
    }
}

#[test]
fn integer_overflow_traps() {
    let mut h = load();
    let i = Value::I32;
    let u = |x: u32| Value::I32(x as i32);
    returns(&mut h, "add_i32", &[i(2), i(3)], i(5));
    traps(&mut h, "add_i32", &[i(i32::MAX), i(1)]);
    traps(&mut h, "add_i32", &[i(i32::MIN), i(-1)]);
    returns(&mut h, "sub_u32", &[u(5), u(3)], u(2));
    traps(&mut h, "sub_u32", &[u(3), u(5)]);
    traps(&mut h, "mul_i32", &[i(65536), i(65536)]);
    returns(&mut h, "mul_i32", &[i(-46341), i(46340)], i(-2_147_441_940));
    traps(&mut h, "mul_i64", &[Value::I64(1 << 32), Value::I64(1 << 31)]);
    returns(
        &mut h,
        "mul_i64",
        &[Value::I64(-(1 << 31)), Value::I64(1 << 32)],
        Value::I64(i64::MIN),
    );
    traps(&mut h, "neg_i32", &[i(i32::MIN)]);
    returns(&mut h, "neg_i32", &[i(i32::MAX)], i(-i32::MAX));
    traps(&mut h, "div_i32", &[i(1), i(0)]);
    traps(&mut h, "div_i32", &[i(i32::MIN), i(-1)]);
    returns(&mut h, "div_i32", &[i(-7), i(2)], i(-3));
    traps(&mut h, "rem_u32", &[u(7), u(0)]);
    returns(&mut h, "rem_u32", &[u(7), u(4)], u(3));
    traps(&mut h, "shl_u32", &[u(1), u(32)]);
    returns(&mut h, "shl_u32", &[u(1), u(31)], u(1 << 31));
    traps(&mut h, "add_u8", &[u(200), u(56)]);
    returns(&mut h, "add_u8", &[u(200), u(55)], u(255));
}

#[test]
fn wrapping_and_conversions() {
    let mut h = load();
    let i = Value::I32;
    let f = Value::F32;
    returns(&mut h, "wrapping_add_i32", &[i(i32::MAX), i(1)], i(i32::MIN));
    returns(&mut h, "wrapping_mul_u32", &[i(-1), i(-1)], i(1));
    returns(&mut h, "bits", &[i(-1)], i(-1));
    returns(&mut h, "to_i32", &[f(-3.7)], i(-3));
    returns(&mut h, "to_i32", &[f(2147483520.0)], i(2147483520));
    traps(&mut h, "to_i32", &[f(2147483648.0)]);
    traps(&mut h, "to_i32", &[f(f32::NAN)]);
    traps(&mut h, "to_u32", &[f(-1.0)]);
    returns(&mut h, "to_u32", &[f(-0.5)], i(0));
    returns(&mut h, "index", &[i(2)], f(3.0));
    traps(&mut h, "index", &[i(3)]);
}

#[test]
fn a_trap_says_where_it_happened() {
    let mut h = load();
    let mut at = |name: &str, args: &[Value]| match h.call_export(name, args) {
        Err(Error::Trap(why)) => why,
        other => panic!("{name}{args:?} should trap, got {other:?}"),
    };
    // `a + b` is line 7 of main.wrela, `a / b` line 27.
    let overflow = at("add_i32", &[Value::I32(i32::MAX), Value::I32(1)]);
    assert!(overflow.ends_with(" at main.wrela:7:5"), "{overflow}");
    let zero = at("div_i32", &[Value::I32(1), Value::I32(0)]);
    assert!(zero.ends_with(" at main.wrela:27:5"), "{zero}");
}

#[test]
fn float_remainder_is_exact() {
    let mut h = load();
    let f32_edges = [
        0.0,
        -0.0,
        f32::from_bits(1),
        1e-40,
        0.5,
        1.0,
        -1.0,
        3.0,
        -7.0,
        6.2831855,
        13177699.0,
        -6263659.0,
        1e30,
        f32::MAX,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
    ];
    let same = |a: f32, b: f32| a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan());
    let mut rng = Rng::new(9);
    let mut pairs: Vec<(f32, f32)> =
        f32_edges.iter().flat_map(|&a| f32_edges.iter().map(move |&b| (a, b))).collect();
    // Quotients of every size, both signs.
    for _ in 0..2000 {
        let scale = |rng: &mut Rng| {
            ((rng.unit() * 2.0 - 1.0) * 10f64.powf(rng.unit() * 60.0 - 30.0)) as f32
        };
        pairs.push((scale(&mut rng), scale(&mut rng)));
    }
    for (a, b) in pairs {
        let got = match h.call_export("rem_f32", &[Value::F32(a), Value::F32(b)]) {
            Ok(v) if v.len() == 1 => match v[0] {
                Value::F32(x) => x,
                ref other => panic!("rem_f32 returned {other:?}"),
            },
            other => panic!("rem_f32({a:e}, {b:e}) gave {other:?}"),
        };
        assert!(same(got, a % b), "{a:e} % {b:e} = {got:e}, want {:e}", a % b);
    }
    for (a, b) in [(13177699.0f64, std::f64::consts::TAU), (1e300, 3.0), (-5.5, 2.0), (1.0, 0.0)] {
        let want = Value::F64(a % b);
        let got = h.call_export("rem_f64", &[Value::F64(a), Value::F64(b)]).expect("rem_f64");
        let ok = match (&got[..], want) {
            ([Value::F64(x)], Value::F64(y)) => {
                x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan())
            }
            _ => false,
        };
        assert!(ok, "{a:e} % {b:e} = {got:?}, want {want:?}");
    }
    let f = Value::F32;
    let got = h
        .call_export("rem_vec3", &[f(13177699.0), f(-7.0), f(5.5), f(6.2831855), f(3.0), f(-2.0)])
        .expect("rem_vec3");
    assert_eq!(got, [f(13177699.0 % 6.2831855), f(-1.0), f(1.5)]);
}

#[test]
fn small_export_parameters_are_brought_into_range() {
    let mut h = load();
    let i = Value::I32;
    // As a conversion to the type would: the low bits.
    returns(&mut h, "widen_u8", &[i(300)], i(44));
    returns(&mut h, "widen_u8", &[i(-1)], i(255));
    returns(&mut h, "widen_i8", &[i(200)], i(-56));
    returns(&mut h, "widen_u16", &[i(65537)], i(1));
    returns(&mut h, "widen_i16", &[i(40000)], i(-25536));
    // Any nonzero `bool` is true.
    returns(&mut h, "is_true", &[i(5)], i(1));
    returns(&mut h, "is_true", &[i(0)], i(0));
}
