//! std's CPU math (compiler/std/math.wrela, f64 internally and rounded once) against Rust's
//! f64 functions rounded to f32: within one ulp everywhere sampled. The interval
//! interpretation's CPU widening (four ulps for these) relies on it.

use std::path::PathBuf;
use wrela_host::{Host, Options, Value};
use wrela_tests::{build, root};

fn load() -> Host {
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("math");
    if let Err(e) = build(&root().join("compiler/tests/math"), &out) {
        panic!("the math package doesn't build:\n{e}");
    }
    Host::load_with(&out, &Options::default()).expect("load")
}

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

struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[test]
fn std_math_is_within_an_ulp() {
    let mut host = load();
    let mut rng = Rng(42);
    type One = fn(f64) -> f64;
    let ones: [(&str, One, f64, f64); 11] = [
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
            let x = if i == 0 { 0.0 } else { x };
            let got = match host.call_export(name, &[Value::F32(x)]).expect(name).as_slice() {
                [Value::F32(y)] => *y,
                other => panic!("{name} returned {other:?}"),
            };
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
        let got = |host: &mut Host, n: &str, a: f32, b: f32| match host
            .call_export(n, &[Value::F32(a), Value::F32(b)])
            .expect(n)
            .as_slice()
        {
            [Value::F32(r)] => *r,
            other => panic!("{n} returned {other:?}"),
        };
        let p = got(&mut host, "f_pow", x, y);
        let u = ulps(p, f64::from(x).powf(f64::from(y)) as f32);
        assert!(u <= 1.0, "pow({x:e}, {y:e}) = {p:e}: {u} ulps");
        worst.0 = worst.0.max(u);
        let (b, a) = ((rng.unit() * 8.0 - 4.0) as f32, (rng.unit() * 8.0 - 4.0) as f32);
        let t = got(&mut host, "f_atan2", b, a);
        let u = ulps(t, f64::from(b).atan2(f64::from(a)) as f32);
        assert!(u <= 1.0, "atan2({b:e}, {a:e}) = {t:e}: {u} ulps");
        worst.1 = worst.1.max(u);
    }
    report.push(format!("  f_pow: worst {:.2} ulp; f_atan2: worst {:.2} ulp", worst.0, worst.1));
    println!("std math against f64 references:\n{}", report.join("\n"));
}
