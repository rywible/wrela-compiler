//! std::fmt's float formatting is exact: wrela's `{x}` (the shortest text that reads back as
//! `x`) and `{x:.p}` (the exact value rounded to `p` places, ties to even) agree with Rust's,
//! byte for byte, for random `f32` and `f64` bit patterns and the edge cases.

use crate::built;
use wrela_host::{CpuHost, Value};
use wrela_tests::Rng;

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    h ^ s.len() as u64
}

/// What wrela wrote, byte by byte through `byte_export`, to show a mismatch.
fn wrela_text(host: &mut CpuHost, byte_export: &str, x: Value, prec: i32) -> String {
    let mut out = Vec::new();
    for i in 0..400 {
        let v = host.call_export(byte_export, &[x, Value::I32(prec), Value::I32(i)]).expect("call");
        match v.as_slice() {
            [Value::I32(0)] => break,
            [Value::I32(b)] => out.push(*b as u8),
            other => panic!("{other:?}"),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn check(
    host: &mut CpuHost,
    export: &str,
    x: Value,
    prec: i32,
    want: &str,
    failures: &mut Vec<String>,
) {
    let got = host.call_export(export, &[x, Value::I32(prec)]).expect("call");
    if got != [Value::I64(fnv(want) as i64)] {
        let bytes = export.replace("_hash", "_byte");
        let text = wrela_text(host, &bytes, x, prec);
        failures.push(format!("{x:?} with precision {prec}: wrela wrote `{text}`, Rust `{want}`"));
    }
}

fn edge_f32() -> Vec<f32> {
    vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.1,
        0.2,
        0.3,
        1.5,
        2.5,
        0.125,
        0.375,
        100.0,
        1e7,
        1e10,
        1e21,
        1e-7,
        3.4028235e38,
        -3.4028235e38,
        f32::MIN_POSITIVE,
        1e-45,
        1.4e-45,
        f32::EPSILON,
        123456.79,
        0.000123,
        9.999999,
        99.5,
        0.05,
        0.15,
        0.25,
        0.35,
        16777216.0,
        16777217.0,
        2.0f32.powi(100),
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
    ]
}

#[test]
fn f32_formatting_matches_rust() {
    let mut host = CpuHost::load(built("format")).expect("load");
    let mut rng = Rng::new(7);
    let mut values = edge_f32();
    for _ in 0..wrela_tests::sized(3000, 20000) {
        values.push(f32::from_bits(rng.next_u64() as u32));
    }
    let mut failures = Vec::new();
    for &x in &values {
        check(&mut host, "f32_hash", Value::F32(x), -1, &format!("{x}"), &mut failures);
        for p in [0, 1, 2, 3, 6] {
            check(
                &mut host,
                "f32_hash",
                Value::F32(x),
                p,
                &format!("{x:.*}", p as usize),
                &mut failures,
            );
        }
        if failures.len() > 10 {
            break;
        }
    }
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn f64_formatting_matches_rust() {
    let mut host = CpuHost::load(built("format")).expect("load");
    let mut rng = Rng::new(11);
    let mut values: Vec<f64> = edge_f32().into_iter().map(f64::from).collect();
    values.extend([
        0.1,
        1e300,
        1e-300,
        5e-324,
        f64::MAX,
        f64::MIN_POSITIVE,
        0.1 + 0.2,
        1e23,
        9007199254740993.0,
    ]);
    for _ in 0..wrela_tests::sized(500, 4000) {
        values.push(f64::from_bits(rng.next_u64()));
    }
    let mut failures = Vec::new();
    for &x in &values {
        check(&mut host, "f64_hash", Value::F64(x), -1, &format!("{x}"), &mut failures);
        for p in [0, 2, 5] {
            check(
                &mut host,
                "f64_hash",
                Value::F64(x),
                p,
                &format!("{x:.*}", p as usize),
                &mut failures,
            );
        }
        if failures.len() > 10 {
            break;
        }
    }
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn integers_match_rust() {
    let mut host = CpuHost::load(built("format")).expect("load");
    let mut rng = Rng::new(3);
    for x in [0, 1, -1, i64::MIN, i64::MAX, 10, -10, 1_000_000_007]
        .into_iter()
        .chain((0..500).map(|_| rng.next_u64() as i64))
    {
        let got = host.call_export("i64_hash", &[Value::I64(x)]).expect("call");
        assert_eq!(got, [Value::I64(fnv(&x.to_string()) as i64)], "{x}");
    }
}
