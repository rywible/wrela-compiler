//! std's CPU math (compiler/std/math.wrela, f64 internally and rounded once) against Rust's
//! f64 functions rounded to f32: within one ulp everywhere sampled, and the same bits at zeros,
//! infinities and NaNs. The interval interpretation's CPU widening (four ulps for these) relies
//! on it. Also std's fields (compiler/std/field.wrela) against references, at the edge cases
//! where a formula breaks down: `k = 0`, coincident or nested spheres, centres, and coordinates
//! past the i32 range.

use crate::built;
use wrela_host::{CpuHost, Value};
use wrela_tests::{Rng, one_f32};

/// The distance between two f32s in units in the last place of the reference.
fn ulps(got: f32, want: f32) -> f64 {
    if got == want || (got.is_nan() && want.is_nan()) {
        return 0.0;
    }
    if !got.is_finite() || !want.is_finite() {
        return f64::INFINITY;
    }
    let ulp = (f64::from(want.abs()) * f64::from(f32::EPSILON)).max(f64::from(f32::from_bits(1)));
    (f64::from(got) - f64::from(want)).abs() / ulp
}

#[test]
fn std_math_is_within_an_ulp() {
    let mut host = CpuHost::load(built("math")).expect("load");
    let mut rng = Rng::new(42);
    type One = fn(f64) -> f64;
    let ones: [(&str, One, f64, f64); 20] = [
        ("f_sinh", f64::sinh, -90.0, 90.0),
        ("f_sinh", f64::sinh, -1.0, 1.0),
        ("f_cosh", f64::cosh, -90.0, 90.0),
        ("f_cosh", f64::cosh, -1.0, 1.0),
        ("f_tanh", f64::tanh, -12.0, 12.0),
        ("f_tanh", f64::tanh, 1e-30, 1.0),
        ("f_sin", f64::sin, -1e4, 1e4),
        ("f_cos", f64::cos, -1e4, 1e4),
        ("f_tan", f64::tan, -1e2, 1e2),
        ("f_exp", f64::exp, -87.0, 88.0),
        ("f_exp2", f64::exp2, -126.0, 127.0),
        ("f_log", f64::ln, 1e-30, 1e30),
        ("f_log2", f64::log2, 1e-30, 1e30),
        ("f_atan", f64::atan, -1e3, 1e3),
        ("f_asin", f64::asin, -1.0, 1.0),
        ("f_acos", f64::acos, -1.0, 1.0),
        ("f_sin", f64::sin, -4.0, 4.0),
        // Every magnitude, and either sign: the range reduction past 2^20 is exact.
        ("f_sin", f64::sin, 1.0, 3.4e38),
        ("f_cos", f64::cos, 1.0, 3.4e38),
        ("f_tan", f64::tan, 1.0, 3.4e38),
    ];
    let mut report = Vec::new();
    for (name, f, lo, hi) in ones {
        let mut worst = 0.0f64;
        for i in 0..20_000 {
            // Log-uniform over positive ranges that span decades, uniform otherwise.
            let x = if lo > 0.0 {
                (lo.ln() + (hi.ln() - lo.ln()) * rng.unit()).exp()
            } else {
                lo + (hi - lo) * rng.unit()
            } as f32;
            // The trigonometric functions' whole range takes both signs.
            let x = if hi > 1e30 && i % 2 == 1 { -x } else { x };
            let x = if i == 0 { 0.0 } else { x };
            let got = one_f32(&mut host, name, &[Value::F32(x)]);
            let want = f(f64::from(x)) as f32;
            let u = ulps(got, want);
            assert!(u <= 1.0, "{name}({x:e}) = {got:e}, want {want:e}: {u} ulps");
            worst = worst.max(u);
        }
        report.push(format!("  {name}: worst {worst:.2} ulp over [{lo:e}, {hi:e}]"));
    }
    let mut worst = (0.0f64, 0.0f64);
    for _ in 0..20_000 {
        let (x, y) = ((rng.unit() * 8.0) as f32, (rng.unit() * 16.0 - 8.0) as f32);
        let p = one_f32(&mut host, "f_pow", &[Value::F32(x), Value::F32(y)]);
        let u = ulps(p, f64::from(x).powf(f64::from(y)) as f32);
        assert!(u <= 1.0, "pow({x:e}, {y:e}) = {p:e}: {u} ulps");
        worst.0 = worst.0.max(u);
        let (b, a) = ((rng.unit() * 8.0 - 4.0) as f32, (rng.unit() * 8.0 - 4.0) as f32);
        let t = one_f32(&mut host, "f_atan2", &[Value::F32(b), Value::F32(a)]);
        let u = ulps(t, f64::from(b).atan2(f64::from(a)) as f32);
        assert!(u <= 1.0, "atan2({b:e}, {a:e}) = {t:e}: {u} ulps");
        worst.1 = worst.1.max(u);
    }
    report.push(format!("  f_pow: worst {:.2} ulp; f_atan2: worst {:.2} ulp", worst.0, worst.1));
    println!("std math against f64 references:\n{}", report.join("\n"));
}

/// The same bits, or both NaN.
fn same(got: f32, want: f32) -> bool {
    got.to_bits() == want.to_bits() || (got.is_nan() && want.is_nan())
}

#[test]
fn std_math_edges_match_c() {
    let mut host = CpuHost::load(built("math")).expect("load");
    let tiny = f32::from_bits(1);
    let edges = [
        0.0,
        -0.0,
        tiny,
        -tiny,
        1e-5,
        -1e-5,
        0.5,
        -0.5,
        1.0,
        -1.0,
        2.0,
        -2.0,
        3.0,
        -3.0,
        2.5,
        1e10,
        -1e10,
        f32::MAX,
        -f32::MAX,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
    ];
    type One = fn(f64) -> f64;
    let ones: [(&str, One); 10] = [
        ("f_sin", f64::sin),
        ("f_cos", f64::cos),
        ("f_tan", f64::tan),
        ("f_exp", f64::exp),
        ("f_exp2", f64::exp2),
        ("f_log", f64::ln),
        ("f_log2", f64::log2),
        ("f_atan", f64::atan),
        ("f_asin", f64::asin),
        ("f_acos", f64::acos),
    ];
    // Zeros, infinities and NaNs exactly; anything else within an ulp.
    let check = |what: String, got: f32, want: f32| {
        let special = want == 0.0 || !want.is_finite();
        assert!(
            if special { same(got, want) } else { ulps(got, want) <= 1.0 },
            "{what} = {got:e}, want {want:e}"
        );
    };
    for (name, f) in ones {
        for &x in &edges {
            let got = one_f32(&mut host, name, &[Value::F32(x)]);
            check(format!("{name}({x:e})"), got, f(f64::from(x)) as f32);
        }
    }
    for &x in &edges {
        for &y in &edges {
            let (a, b) = (Value::F32(x), Value::F32(y));
            let got = one_f32(&mut host, "f_pow", &[a, b]);
            check(format!("pow({x:e}, {y:e})"), got, f64::from(x).powf(f64::from(y)) as f32);
            let got = one_f32(&mut host, "f_atan2", &[a, b]);
            check(format!("atan2({x:e}, {y:e})"), got, f64::from(x).atan2(f64::from(y)) as f32);
        }
    }
}

/// Calls export `name` with f32 arguments.
fn call(host: &mut CpuHost, name: &str, args: &[f32]) -> Vec<f32> {
    let args: Vec<Value> = args.iter().map(|&x| Value::F32(x)).collect();
    host.call_export(name, &args)
        .expect(name)
        .into_iter()
        .map(|v| match v {
            Value::F32(x) => x,
            other => panic!("{name} returned {other:?}"),
        })
        .collect()
}

/// The signed distance to the hull of two spheres (a round cone), in f64: the hull is the union
/// of the spheres between them, `c(t) = a + t (b - a)` with radius `r1 + t (r2 - r1)`, and
/// `|p - c(t)| - r(t)` is convex in `t`, so a ternary search finds its minimum, which is the
/// signed distance inside as well as out.
fn round_cone_reference(a: [f32; 3], b: [f32; 3], r1: f32, r2: f32, p: [f32; 3]) -> f64 {
    let f = |t: f64| {
        let d: f64 = (0..3)
            .map(|i| {
                let (a, b) = (f64::from(a[i]), f64::from(b[i]));
                (f64::from(p[i]) - (a + t * (b - a))).powi(2)
            })
            .sum();
        d.sqrt() - (f64::from(r1) + t * f64::from(r2 - r1))
    };
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..200 {
        let (m1, m2) = (lo + (hi - lo) / 3.0, hi - (hi - lo) / 3.0);
        if f(m1) < f(m2) {
            hi = m2;
        } else {
            lo = m1;
        }
    }
    f(0.0).min(f(1.0)).min(f(0.5 * (lo + hi)))
}

#[test]
fn std_fields_hold_at_their_edge_cases() {
    let mut host = CpuHost::load(built("math")).expect("load");
    let mut rng = Rng::new(7);
    let mut r = |lo: f64, hi: f64| (lo + (hi - lo) * rng.unit()) as f32;

    // smin: `k = 0` (or less) is `min`, not NaN; a smooth union with `k = 0` is a union.
    for k in [0.0, -0.0, -1.0] {
        for (a, b) in [(1.0f32, 2.0f32), (2.0, 1.0), (1.0, 1.0), (-3.0, 0.5)] {
            let got = call(&mut host, "f_smin", &[a, b, k])[0];
            assert_eq!(got, a.min(b), "smin({a}, {b}, {k})");
        }
    }
    assert_eq!(call(&mut host, "f_blend", &[0.0, 0.0, 0.0, 0.0])[0], 0.0);
    assert_eq!(call(&mut host, "f_blend", &[3.0, 0.0, 0.0, 0.0])[0], 1.0);
    // Two empty surfaces (infinite distances) blend to an empty one, and a huge `k` doesn't
    // overflow: on the seam it's `a - k / 4`.
    assert_eq!(call(&mut host, "f_smin", &[f32::INFINITY, f32::INFINITY, 0.1])[0], f32::INFINITY);
    assert_eq!(call(&mut host, "f_smin", &[1.0, 1.0, 1e20])[0], 1.0 - 2.5e19);

    // round_cone is exact everywhere: two spheres apart, one holding the other, the same, or
    // nearly the same (where the usual formula's terms leave f32's range).
    let mut worst = 0.0f64;
    for i in 0..2000 {
        let a = [r(-1.0, 1.0), r(-1.0, 1.0), r(-1.0, 1.0)];
        let near = |a: [f32; 3], d: f32| [a[0] + d, a[1], a[2] - d];
        let b = match i % 10 {
            0 => a,
            1 => near(a, 1e-9),
            2 => near(a, 3e-8),
            3 => near(a, 1e-6),
            _ => [r(-1.0, 1.0), r(-1.0, 1.0), r(-1.0, 1.0)],
        };
        let (r1, r2) = (r(0.0, 1.5), r(0.0, 1.5));
        let p = [r(-2.5, 2.5), r(-2.5, 2.5), r(-2.5, 2.5)];
        let args = [a[0], a[1], a[2], b[0], b[1], b[2], r1, r2, p[0], p[1], p[2]];
        let got = f64::from(call(&mut host, "f_round_cone", &args)[0]);
        let want = round_cone_reference(a, b, r1, r2, p);
        let e = (got - want).abs();
        assert!(e < 1e-5, "round_cone({a:?}, {b:?}, {r1}, {r2}) at {p:?} = {got}, want {want}");
        worst = worst.max(e);
    }
    println!("round_cone: worst error {worst:.2e} over 2000 cases");

    // The ellipsoid's bound is inside (negative) at its centre too: the smallest radius there.
    assert_eq!(call(&mut host, "f_ellipsoid", &[1.0, 2.0, 3.0, 0.0, 0.0, 0.0])[0], -1.0);
    assert_eq!(call(&mut host, "f_ellipsoid", &[1.0, 2.0, 3.0, -0.0, 0.0, -0.0])[0], -1.0);
    assert!(call(&mut host, "f_ellipsoid", &[1.0, 2.0, 3.0, 1e-20, 0.0, 0.0])[0] < 0.0);

    // half_space is exact for a normal of any length, and keeps the same half-space.
    // Also one whose length's square would underflow or overflow.
    let planes: [([f32; 3], f32); 6] = [
        ([0.0, 2.0, 0.0], 1.0),
        ([3.0, 0.0, 4.0], -5.0),
        ([0.0, -0.5, 0.0], 0.0),
        ([0.0, 1e-23, 0.0], 0.0),
        ([3e-30, 0.0, 4e-30], 1e-30),
        ([0.0, 1e20, 1e20], 2e20),
    ];
    for (n, offset) in planes {
        let n64 = n.map(f64::from);
        let len = (n64[0] * n64[0] + n64[1] * n64[1] + n64[2] * n64[2]).sqrt();
        for p in [[0.0, 0.0, 0.0], [1.0, 2.0, 3.0], [-4.0, 0.5, 2.0]] {
            let got =
                call(&mut host, "f_half_space", &[n[0], n[1], n[2], offset, p[0], p[1], p[2]]);
            let p64 = p.map(f64::from);
            let want =
                (n64[0] * p64[0] + n64[1] * p64[1] + n64[2] * p64[2] - f64::from(offset)) / len;
            let e = (f64::from(got[0]) - want).abs();
            assert!(e < 1e-6, "half_space({n:?}, {offset}) at {p:?}: {got:?}, want {want}");
        }
    }

    // Value noise and fbm take any coordinate: past the i32 range a lattice coordinate wraps
    // to 32 bits, as the hash does (so 2^31 and -2^31 are one plane), and inf or NaN gives NaN.
    let big = 2147483648.0f32;
    for (y, z) in [(0.25, 0.5), (-7.5, 1e9)] {
        let a = call(&mut host, "f_value_noise", &[big, y, z])[0];
        let b = call(&mut host, "f_value_noise", &[-big, y, z])[0];
        assert_eq!(a, b, "value noise at x = 2^31 and -2^31");
        assert!((-1.0..=1.0).contains(&a), "{a}");
    }
    for x in [3e9, -3e9, 1e30, f32::MAX, -f32::MAX] {
        let v = call(&mut host, "f_value_noise", &[x, 0.5, 0.5])[0];
        assert!((-1.0..=1.0).contains(&v), "value noise at {x:e}: {v}");
        // fbm's octaves scale the point by up to 8 here, so only where that stays finite.
        if x.abs() < 1e37 {
            let f = call(&mut host, "f_fbm", &[x, 0.5, 0.5])[0];
            assert!(f.abs() <= 2.0, "fbm at {x:e}: {f}");
        }
    }
    for x in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
        assert!(call(&mut host, "f_value_noise", &[x, 0.5, 0.5])[0].is_nan(), "{x}");
        assert!(call(&mut host, "f_fbm", &[0.5, x, 0.5])[0].is_nan(), "{x}");
    }
}
