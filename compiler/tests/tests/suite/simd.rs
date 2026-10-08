//! AC12, language.md §11: vector math uses WASM's 128-bit SIMD, and the bits are the same as
//! without. Each program here is built twice, with SIMD and without
//! (`wrela_driver::build_without_simd`), and the two must agree exactly:
//!
//! - compiler/tests/simd: 40 cases of every vector operation, on vectors whose components are
//!   special values (zeros of both signs, extremes, a subnormal, infinities, NaN) or random;
//! - the numerics corpus's exports, on sampled arguments;
//! - the command-stream hash (every byte a program submits to the host, not its state) of
//!   programs run for some frames, the samples the audio test's voice renders, and sketch 03's
//!   world hashes (`StateHash`, its state's) tick by tick;
//! - loops four iterations at a time, at every length to 64, and one that traps;
//! - the run-pass suite's expectations (`run_pass.rs` checks both builds).
//!
//! The speed-up on a loop of `vec4` math is measured, not gated: `cargo test --release -p
//! wrela-tests --test suite simd_speed -- --ignored --nocapture` prints it.

use std::path::Path;
use wrela_host::{CpuBuild, CpuHost, Error, Value, frame_time};
use wrela_tests::{median, one_u32, page, scalar_page, sized};

fn simd_instructions(dir: &Path) -> usize {
    let wasm = std::fs::read(dir.join("game.wasm")).expect("game.wasm");
    wrela_wasm::simd_instructions(&wasm).expect("valid WASM")
}

/// The program in `pkg` built both ways: (with SIMD, without), in directories of their own
/// for `test` (tests run at once).
fn both(pkg: &str, test: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let name = format!("{}-{test}", pkg.replace('/', "-"));
    let (simd, _) = page(pkg, &format!("simd-on-{name}"));
    let (scalar, _) = scalar_page(pkg, &format!("simd-off-{name}"));
    assert_eq!(simd_instructions(&scalar), 0, "{pkg}: the build without SIMD has SIMD");
    (simd, scalar)
}

#[test]
fn every_vector_operation_has_the_same_bits_with_simd() {
    let (simd, scalar) = both("compiler/tests/simd", "ops");
    assert!(simd_instructions(&simd) > 500, "the SIMD build has little SIMD");
    let mut a = CpuHost::load(&simd).expect("load");
    let mut b = CpuHost::load(&scalar).expect("load");
    let cases = one_u32(&mut a, "cases", &[]);
    let seeds: u32 = sized(2_000, 50_000);
    let chunk: u32 = 500;
    for from in (0..seeds).step_by(chunk as usize) {
        let args = [Value::I32(from as i32), Value::I32(chunk as i32)];
        let x = a.call_export("all", &args).expect("all");
        if x == b.call_export("all", &args).expect("all") {
            continue;
        }
        // Which case: the first that differs.
        for seed in from..from + chunk {
            for op in 0..cases {
                let args = [Value::I32(op as i32), Value::I32(seed as i32)];
                let x = a.call_export("case", &args).expect("case");
                let y = b.call_export("case", &args).expect("case");
                assert_eq!(x, y, "case {op}, seed {seed}: SIMD and not");
            }
        }
        panic!("seeds {from}.. differ, though no case does");
    }
}

/// Loops four iterations at a time (language.md §11): every length from 0 to 64, so each
/// remainder; and a loop that indexes a run past its end, which traps at the same iteration,
/// with the same elements written before it.
#[test]
fn loops_four_at_a_time_have_the_same_bits() {
    let (simd, scalar) = both("compiler/tests/simd", "loops");
    let mut hosts =
        [&simd, &scalar].map(|d| CpuBuild::load(d).expect("load").start_with(1).expect("start"));
    let seeds: i32 = sized(40, 400);
    for seed in 0..seeds {
        for n in 0..=64 {
            let args = [Value::I32(seed), Value::I32(n)];
            let [a, b] = &mut hosts;
            let (x, y) = (a.call_export("loops", &args), b.call_export("loops", &args));
            assert!(same(&x, &y), "loops({seed}, {n}): {x:?} with SIMD, {y:?} without");
        }
    }
    for n in [0, 3, 4, 36, 39, 40, 41, 43, 44, 45, 64] {
        let [a, b] = &mut hosts;
        let args = [Value::I32(n)];
        let (x, y) = (a.call_export("short", &args), b.call_export("short", &args));
        assert!(same(&x, &y), "short({n}): {x:?} with SIMD, {y:?} without");
        assert_eq!(x.is_err(), n > 40, "short({n}): {x:?}");
        // What a trap leaves written.
        for h in [&mut *a, &mut *b] {
            h.frame(0.0, 4, 4).expect("frame");
            let _ = h.call_export("short_into", &args);
        }
        let x = a.call_export("short_written", &[]).expect("read");
        let y = b.call_export("short_written", &[]).expect("read");
        assert_eq!(x, y, "short_into({n}) wrote different elements before it stopped");
    }
}

/// The same result: equal values (any NaN is NaN: NaN bits are canonical only where they're
/// observed), or both traps.
fn same(x: &Result<Vec<Value>, Error>, y: &Result<Vec<Value>, Error>) -> bool {
    let value = |a: &Value, b: &Value| match (a, b) {
        (Value::F32(p), Value::F32(q)) => p.to_bits() == q.to_bits() || (p.is_nan() && q.is_nan()),
        (Value::F64(p), Value::F64(q)) => p.to_bits() == q.to_bits() || (p.is_nan() && q.is_nan()),
        _ => a == b,
    };
    match (x, y) {
        (Ok(p), Ok(q)) => p.len() == q.len() && p.iter().zip(q).all(|(a, b)| value(a, b)),
        (Err(Error::Trap(_)), Err(Error::Trap(_))) => true,
        _ => false,
    }
}

#[test]
fn the_numerics_corpus_is_the_same_with_simd() {
    let (simd, scalar) = both("compiler/tests/numerics", "numerics");
    let mut a = CpuHost::load(&simd).expect("load");
    let mut b = CpuHost::load(&scalar).expect("load");
    let mut rng = wrela_tests::Rng::new(12);
    let floats = [0.0f32, -0.0, 1.5, -7.25, 1e-40, 3e38, f32::INFINITY, f32::NAN];
    for (name, params, _) in a.exports() {
        if name == "frame" {
            continue;
        }
        for k in 0..200 {
            let args: Vec<Value> = params
                .iter()
                .map(|t| match *t {
                    "i64" => Value::I64(rng.next_u64() as i64 >> (rng.next_u64() % 64)),
                    "f32" if k % 3 == 0 => Value::F32(floats[(rng.next_u64() % 8) as usize]),
                    "f32" => Value::F32(f32::from_bits(rng.next_u64() as u32)),
                    "f64" => Value::F64(f64::from_bits(rng.next_u64())),
                    _ => Value::I32(rng.next_u64() as i32 >> (rng.next_u64() % 32)),
                })
                .collect();
            let (x, y) = (a.call_export(&name, &args), b.call_export(&name, &args));
            assert!(same(&x, &y), "{name}{args:?}: {x:?} with SIMD, {y:?} without");
        }
    }
}

/// Programs run for some frames, both ways: the same bytes submitted, frame by frame.
#[test]
fn command_streams_are_the_same_with_simd() {
    let programs = [
        ("examples/hello-field", 30),
        ("compiler/tests/sketches/01-creature", 10),
        ("compiler/tests/sketches/02-drawing", 4),
        ("compiler/tests/sketches/03-simulation", 60),
        ("compiler/tests/sketches/gameplay", 30),
        ("compiler/tests/parallel", 20),
        ("compiler/tests/snapshots", 30),
        ("compiler/tests/renderer", 4),
        ("compiler/tests/render", 4),
        ("compiler/tests/fields", 4),
        ("compiler/tests/noise", 4),
        ("compiler/tests/channels", 4),
        ("compiler/tests/lipschitz", 4),
    ];
    wrela_tests::par_each(
        &programs,
        || (),
        |(), &(pkg, frames)| {
            let (simd, scalar) = both(pkg, "hashes");
            let mut hosts = [simd, scalar]
                .map(|d| CpuBuild::load(&d).expect("load").start_with(1).expect("start"));
            for i in 0..frames {
                for h in &mut hosts {
                    h.frame(frame_time(i, 60.0), 64, 48).expect("frame");
                }
                let [x, y] = &hosts;
                assert_eq!(x.hash(), y.hash(), "{pkg}: frame {i} differs with SIMD");
            }
        },
    );
}

/// Sketch 03's world, ticked both ways: the same `StateHash` at every tick.
#[test]
fn world_hashes_are_the_same_with_simd() {
    let (simd, scalar) = both("compiler/tests/sketches/03-simulation", "world");
    let hashes = |dir: &Path| -> Vec<Vec<Value>> {
        let mut host = CpuHost::load(dir).expect("load");
        host.call_export("run", &[Value::I32(120)]).expect("run");
        (0..120).map(|t| host.call_export("checksum_at", &[Value::I32(t)]).expect("hash")).collect()
    };
    let (x, y) = (hashes(&simd), hashes(&scalar));
    let first = x.iter().zip(&y).position(|(a, b)| !same(&Ok(a.clone()), &Ok(b.clone())));
    assert_eq!(first, None, "the world's hash differs with SIMD from this tick on");
}

#[test]
fn a_voice_renders_the_same_samples_with_simd() {
    let (simd, scalar) = both("compiler/tests/audio", "voice");
    let mut hosts =
        [simd, scalar].map(|d| CpuBuild::load(&d).expect("load").start_with(1).expect("start"));
    for h in &mut hosts {
        h.frame(0.0, 16, 16).expect("frame");
    }
    let [a, b] = &mut hosts;
    let (x, y) = (a.render_audio(200).expect("render"), b.render_audio(200).expect("render"));
    let bits = |s: &[f32]| s.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    assert!(bits(&x) == bits(&y), "the samples differ with SIMD");
}

/// Measured, not gated: a loop of `vec4` math (compiler/tests/simd's `vec4_math`), and `f32`
/// loops four iterations at a time (`f32_loop`), with SIMD and without.
#[test]
#[ignore = "measure: reported, not gated; run it with --release"]
fn simd_speed() {
    let (simd, scalar) = both("compiler/tests/simd", "speed");
    let time = |dir: &Path, name: &str, n: i32| {
        let mut host = CpuHost::load(dir).expect("load");
        let args = [Value::I32(n)];
        let first = host.call_export(name, &args).expect(name); // warm-up
        let runs: Vec<f64> = (0..9)
            .map(|_| {
                let t = std::time::Instant::now();
                let r = host.call_export(name, &args).expect(name);
                assert_eq!(r, first);
                t.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        (median(&runs), first)
    };
    let ((on, x), (off, y)) = (time(&simd, "vec4_math", 2000), time(&scalar, "vec4_math", 2000));
    assert_eq!(x, y, "the result differs with SIMD");
    println!(
        "a loop of vec4 math (256 particles, 2000 steps): {on:.2} ms with SIMD, {off:.2} ms without: {:.2}x",
        off / on
    );
    let ((on, x), (off, y)) = (time(&simd, "f32_loop", 100), time(&scalar, "f32_loop", 100));
    assert_eq!(x, y, "the result differs with SIMD");
    println!(
        "f32 loops four iterations at a time (4096 elements, 100 passes): {on:.2} ms with SIMD, {off:.2} ms without: {:.2}x",
        off / on
    );
}
