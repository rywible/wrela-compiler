//! AC7: strict CPU numerics, run under wasmtime. Integer arithmetic that overflows, divides by
//! zero or shifts by the type's width or more traps; wrapping arithmetic and integer-to-integer
//! conversions keep the low bits; float-to-integer conversions truncate, and trap out of range
//! or on NaN; an index out of range traps. (The state hash agreeing between Chrome and wasmtime
//! is tests/hello_field.rs; the module check for relaxed SIMD is wrela-wasm's.)

use std::path::PathBuf;
use wrela_host::{CpuHost, Error, Value};
use wrela_tests::built;

fn load() -> CpuHost {
    let out = built("numerics", PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("numerics"));
    CpuHost::load(out).expect("load")
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
