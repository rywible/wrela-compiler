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

/// A debug build traps where a float operation creates a NaN, and a release build doesn't pay
/// for the check (§11). A NaN passed on isn't created: it doesn't trap.
#[test]
fn a_debug_build_traps_where_a_nan_is_created() {
    let src = "pub fn divide(a: f32, b: f32) -> f32 {
    a / b
}

pub fn root(x: f32) -> f32 {
    sqrt(x)
}

pub fn spread(x: f32) -> f32 {
    let v = vec3(x, 1.0, 2.0) * 0.0
    v.y
}

pub fn passed_on() -> f32 {
    let nan = bitcast_f32(0x7fc00000)
    nan + 1.0
}

pub fn frame(time: f32, width: u32, height: u32) {}
";
    let dir = crate::package("debug_nans", src);
    let load = |debug: bool, out: &str| {
        let built = if debug { wrela_driver::build_debug(&dir) } else { wrela_driver::build(&dir) };
        assert!(!built.has_errors(), "doesn't build");
        built.write_to(&dir.join(out)).expect("write");
        CpuHost::load(dir.join(out)).expect("load")
    };
    let mut release = load(false, "release");
    let mut debug = load(true, "debug");
    let nan = |host: &mut CpuHost, name: &str, args: &[Value]| match host.call_export(name, args) {
        Ok(v) => matches!(v.as_slice(), [Value::F32(x)] if x.is_nan()),
        Err(_) => false,
    };
    let zero = [Value::F32(0.0), Value::F32(0.0)];
    assert!(nan(&mut release, "divide", &zero));
    assert!(nan(&mut release, "root", &[Value::F32(-1.0)]));
    // The NaN is in a component the function doesn't return.
    returns(&mut release, "spread", &[Value::F32(f32::INFINITY)], Value::F32(0.0));
    for (name, args) in [
        ("divide", &zero[..]),
        ("root", &[Value::F32(-1.0)][..]),
        ("spread", &[Value::F32(f32::INFINITY)][..]),
    ] {
        match debug.call_export(name, args) {
            Err(Error::Trap(t)) => assert!(
                t.starts_with("panic: debug build: a float operation created a NaN"),
                "{name}: {t}"
            ),
            other => panic!("{name} should trap in a debug build, got {other:?}"),
        }
    }
    returns(&mut debug, "divide", &[Value::F32(1.0), Value::F32(2.0)], Value::F32(0.5));
    assert!(nan(&mut debug, "passed_on", &[]));
    // Release builds don't pay: the debug module is larger.
    let size = |out: &str| std::fs::metadata(dir.join(out).join("game.wasm")).expect("wasm").len();
    assert!(size("debug") > size("release"));
}

/// The inputs compiler/tests/bits counts: 0, 1, the top bit, every bit, then xorshift's.
fn bits_input(i: u32) -> u32 {
    if i < 4 {
        return [0, 1, 1 << 31, u32::MAX][i as usize];
    }
    let mut x: u32 = 2463534242 ^ i;
    for _ in 0..3 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
    }
    x >> (i % 29)
}

/// `count_ones`, `leading_zeros` and `trailing_zeros` are Rust's, of `u32`, `i32`, `u64` and
/// `i64` (the width for 0).
#[test]
fn bits_are_counted() {
    let mut h = CpuHost::load(built("bits")).expect("load");
    let u = |x: u32| Value::I32(x as i32);
    for i in 0..256 {
        let x = bits_input(i);
        let wide = (u64::from(x) << 32 | u64::from(x.rotate_left(7))) >> (i % 61);
        for (name, arg, want) in [
            ("ones_u32", u(x), x.count_ones()),
            ("leading_u32", u(x), x.leading_zeros()),
            ("trailing_u32", u(x), x.trailing_zeros()),
            ("ones_i32", u(x), (x as i32).count_ones()),
            ("leading_i32", u(x), (x as i32).leading_zeros()),
            ("trailing_i32", u(x), (x as i32).trailing_zeros()),
            ("ones_u64", Value::I64(wide as i64), wide.count_ones()),
            ("leading_u64", Value::I64(wide as i64), wide.leading_zeros()),
            ("trailing_i64", Value::I64(wide as i64), (wide as i64).trailing_zeros()),
        ] {
            returns(&mut h, name, &[arg], u(want));
        }
    }
}

/// The GPU counts bits as the CPU does (WGSL's `countOneBits` and its kin), of `u32`s and
/// `i32`s: compiler/tests/bits's first frame.
#[test]
#[ignore = "needs a GPU"]
fn the_gpu_counts_bits_as_the_cpu_does() {
    let mut gpu = wrela_host::Host::load(built("bits")).expect("load");
    gpu.frame(0.0, 64, 64).expect("a frame");
    let out = *gpu.buffers().last().expect("a buffer");
    let words = wrela_tests::u32s(&gpu.read_buffer(out).expect("read"));
    for i in 0..256 {
        let x = bits_input(i);
        let want = [x.count_ones(), x.leading_zeros(), x.trailing_zeros()];
        let got = &words[i as usize * 6..i as usize * 6 + 6];
        assert_eq!(got[..3], want, "{x:#010x} as a u32");
        assert_eq!(got[3..], want, "{x:#010x} as an i32");
    }
}

/// Vectors of i32s and u32s compute the same on the CPU (a component at a time) and the GPU
/// (WGSL's vectors): arithmetic, shifts and bits, built-ins, conversions to and from float
/// vectors, and swizzles. compiler/tests/vectors's first frame writes the CPU's values and the
/// GPU's into its two newest buffers.
#[test]
#[ignore = "needs a GPU"]
fn the_gpu_computes_integer_vectors_as_the_cpu_does() {
    let mut gpu = wrela_host::Host::load(built("vectors")).expect("load");
    gpu.frame(0.0, 64, 64).expect("a frame");
    let b = gpu.buffers();
    let (cpu, on_gpu) = (b[b.len() - 2], b[b.len() - 1]);
    let (cpu, on_gpu) =
        (gpu.read_buffer(cpu).expect("read"), gpu.read_buffer(on_gpu).expect("read"));
    assert_eq!(cpu.len(), on_gpu.len());
    assert!(cpu.iter().any(|&x| x != 0), "the CPU's values are all zero");
    let first = cpu.chunks(4).zip(on_gpu.chunks(4)).position(|(a, b)| a != b);
    assert_eq!(first, None, "the CPU's and the GPU's words differ (the first differing word)");
}

/// Vectors of i32s, u32s and f64s on the CPU: compiler/tests/vectors's `@test`s.
#[test]
fn integer_and_double_vectors_compute_on_the_cpu() {
    let out = wrela_driver::test(&wrela_tests::repo_root().join("compiler/tests/vectors"), None);
    let failures: Vec<_> = out.results.iter().filter_map(|r| r.failure.as_ref()).collect();
    assert!(out.passed(), "{:?} {failures:?}", out.diagnostics);
    assert_eq!(out.results.len(), 3);
}

/// A component of an integer vector overflows as a scalar does on the CPU: it traps.
#[test]
fn an_integer_vector_component_traps_on_overflow() {
    let mut h = CpuHost::load(built("vectors")).expect("load");
    traps(&mut h, "overflow", &[]);
}
