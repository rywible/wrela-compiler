//! AC4 and AC5: lifted builds (language.md §22). compiler/tests/lift draws the blob of its
//! dependency `shapes` (std shapes, with literals in every form a lifted build lifts) and
//! exports its distance, the lifted literals and parameter gradients.
//!
//! - Coverage: every float literal of `shapes` is lifted, or `lift.json` says why not.
//! - A lifted build computes what the normal build does, bit for bit, on the CPU; on the GPU
//!   its frame is the same (`--ignored`).
//! - A literal set while the program runs changes what the CPU computes from the next call,
//!   and what the GPU draws from the next frame.
//! - Parameter gradients agree with central differences of the distance (through `set`) at
//!   sample points, and several literals derived at once give each its own derivative.
//! - `wrela edit` with each literal's own value changes nothing; with a new value, the rebuilt
//!   program reads it.

use std::path::{Path, PathBuf};
use wrela_host::{CpuHost, Host, Value, frame_time, image};
use wrela_tests::{one_f32, one_u32, repo_root, sized};

fn package() -> PathBuf {
    repo_root().join("compiler/tests/lift")
}

/// The program, built lifted (`shapes`) or not, into a scratch directory `name`.
fn built(name: &str, lift: bool) -> PathBuf {
    build_into(name, &package(), if lift { &["shapes"] } else { &[] })
}

/// The program at `pkg`, built with the packages `lift` lifted (none: a normal build), into a
/// scratch directory `name`.
pub(crate) fn build_into(name: &str, pkg: &Path, lift: &[&str]) -> PathBuf {
    let dir = super::scratch(name);
    let out = if lift.is_empty() {
        wrela_driver::build(pkg)
    } else {
        let names: Vec<String> = lift.iter().map(|s| s.to_string()).collect();
        wrela_driver::build_lifted(pkg, &names, false).expect("--lift")
    };
    assert!(
        !out.has_errors(),
        "{}",
        wrela_diag::render::render_all(&out.sources, &out.diagnostics)
    );
    out.write_to(&dir).expect("write the build");
    dir
}

pub(crate) fn report(dir: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(dir.join("lift.json")).expect("lift.json");
    serde_json::from_str(&text).expect("lift.json is JSON")
}

fn sample(host: &mut CpuHost, i: u32) -> [f32; 3] {
    match host.call_export("sample", &[Value::I32(i as i32)]).expect("sample").as_slice() {
        [Value::F32(x), Value::F32(y), Value::F32(z)] => [*x, *y, *z],
        other => panic!("sample returned {other:?}"),
    }
}

pub(crate) fn distance(host: &mut CpuHost, p: [f32; 3]) -> f32 {
    one_f32(host, "distance", &[Value::F32(p[0]), Value::F32(p[1]), Value::F32(p[2])])
}

#[test]
fn every_float_literal_is_lifted_or_has_a_reason() {
    let dir = built("lift-coverage", true);
    let r = report(&dir);
    let lifted = r["literals"].as_array().expect("literals");
    let not = r["not_lifted"].as_array().expect("not_lifted");
    let files = r["files"].as_array().expect("files");
    assert_eq!(files.len(), 1, "one lifted file: {files:?}");
    assert_eq!(files[0]["path"], "shapes/blob.wrela");
    // Each not lifted, and why: the computed constant's two, and the f64.
    let why: Vec<(String, String)> = not
        .iter()
        .map(|n| (n["text"].as_str().unwrap().into(), n["why"].as_str().unwrap().into()))
        .collect();
    assert_eq!(why.len(), 3, "{why:#?}");
    assert!(why[0].0 == "0.5" && why[0].1.contains("computes"), "{why:?}");
    assert!(why[1].0 == "0.25" && why[1].1.contains("computes"), "{why:?}");
    assert!(why[2].0 == "2.5" && why[2].1.contains("f64"), "{why:?}");

    // Every float token of the file is one or the other, and each lifted one's text is its
    // value (a unit suffix times its unit, a minus before it negating it).
    let text = std::fs::read_to_string(package().join("shapes/blob.wrela")).unwrap();
    let floats = wrela_syntax::lexer::lex(wrela_diag::FileId(0), &text)
        .tokens
        .iter()
        .filter(|t| {
            use wrela_syntax::token::TokenKind::*;
            matches!(t.kind, Float | Suffixed)
        })
        .count();
    let lifted_floats = lifted
        .iter()
        .filter(|l| {
            let t = l["text"].as_str().unwrap();
            t.contains('.') || t.ends_with("cm") || t.ends_with("mm")
        })
        .count();
    assert_eq!(lifted_floats + not.len(), floats, "each float literal is in the report once");
    for l in lifted {
        let t = l["text"].as_str().unwrap();
        let v = l["value"].as_f64().unwrap() as f32;
        let read = wrela_driver::edit::literal_value(t.trim_start_matches('-')).expect(t);
        let read = if t.starts_with('-') { -read } else { read };
        assert_eq!(read, v, "{t} is {v} in the table");
    }
    // Integers where an f32 is: `vec3(0, 55cm, 0)` lifts its zeros.
    assert!(lifted.iter().any(|l| l["text"] == "0"), "an integer literal used as an f32 is lifted");
    eprintln!("{} literals lifted, {} not", lifted.len(), not.len());
}

#[test]
fn a_lifted_build_computes_what_the_normal_build_does() {
    let mut normal = CpuHost::load(built("lift-same-normal", false)).expect("load");
    let mut lifted = CpuHost::load(built("lift-same-lifted", true)).expect("load");
    assert_eq!(one_u32(&mut normal, "literals", &[]), 0);
    let n = one_u32(&mut lifted, "literals", &[]);
    assert!(n > 60, "{n} literals");
    let points = sized(256, 4096);
    for i in 0..points {
        let p = sample(&mut normal, i);
        assert_eq!(p, sample(&mut lifted, i), "the sample points");
        let (a, b) = (distance(&mut normal, p), distance(&mut lifted, p));
        assert_eq!(a.to_bits(), b.to_bits(), "at {p:?}: normal {a}, lifted {b}");
    }
}

#[test]
fn a_literal_set_while_the_program_runs_changes_what_it_computes() {
    let dir = built("lift-set", true);
    let r = report(&dir);
    let mut host = CpuHost::load(&dir).expect("load");
    // The head's radius, `R` (0.11): a point in front of the head moves with it.
    let lits = r["literals"].as_array().unwrap();
    let i = lits.iter().position(|l| l["text"] == "0.11").expect("R is lifted") as i32;
    assert_eq!(one_f32(&mut host, "value", &[Value::I32(i)]), 0.11);
    let p = [0.0, 0.95, 0.62 + 0.3];
    let before = distance(&mut host, p);
    host.call_export("set", &[Value::I32(i), Value::F32(0.21)]).expect("set");
    assert_eq!(one_f32(&mut host, "value", &[Value::I32(i)]), 0.21);
    let after = distance(&mut host, p);
    assert!((before - after - 0.1).abs() < 0.01, "before {before}, after {after}");
    host.call_export("set", &[Value::I32(i), Value::F32(0.11)]).expect("set");
    assert_eq!(distance(&mut host, p).to_bits(), before.to_bits(), "set back, the same again");
}

/// The distance's derivative by literal `i` at `p`, derived.
pub(crate) fn derived(host: &mut CpuHost, p: [f32; 3], i: u32) -> f64 {
    let args = [Value::F32(p[0]), Value::F32(p[1]), Value::F32(p[2]), Value::I32(i as i32)];
    f64::from(one_f32(host, "derivative", &args))
}

/// Literal `i` set to `x`: the value the table holds.
pub(crate) fn set(host: &mut CpuHost, i: u32, x: f32) -> f64 {
    host.call_export("set", &[Value::I32(i as i32), Value::F32(x)]).unwrap();
    f64::from(one_f32(host, "value", &[Value::I32(i as i32)]))
}

/// Literal `i`'s derivative at `p` against central differences, over `[v − h, v + h]`: the
/// derived derivative there, averaged (Simpson's rule, 17 points), and the central difference.
/// Comparing them over the same interval leaves no truncation error. The step is the smallest
/// whose `f32` rounding is 2·10⁻⁵ (an ulp of the values the distance is computed from, which
/// near the surface are still the blob's size, over the step), whatever the literal's size: a
/// literal's effect needn't scale with it (an offset of 1.5 into noise whose cells are 2 cm).
/// `None` where the derivative doesn't integrate (a kink in the interval: Simpson's rule and
/// the trapezoid rule disagree by half the allowance).
pub(crate) fn against_central(
    host: &mut CpuHost,
    p: [f32; 3],
    i: u32,
    v: f32,
    d: f32,
) -> Option<(f64, f64)> {
    const N: usize = 16;
    let a = d.abs().max(0.25);
    let ulp = f64::from(f32::from_bits(a.to_bits() + 1) - a);
    let h = ulp / 2e-5;
    let mut at = Vec::new();
    for k in 0..=N {
        let x = set(host, i, (f64::from(v) - h + 2.0 * h * k as f64 / N as f64) as f32);
        at.push((x, derived(host, p, i), distance(host, p)));
    }
    set(host, i, v);
    let width = at[N].0 - at[0].0;
    let central = (f64::from(at[N].2) - f64::from(at[0].2)) / width;
    let w = width / N as f64;
    let ends = at[0].1 + at[N].1;
    let inner = |k: usize| at[k].1 * if k % 2 == 1 { 4.0 } else { 2.0 };
    let simpson = w / 3.0 * (ends + (1..N).map(inner).sum::<f64>()) / width;
    let trapezoid = w * (ends / 2.0 + (1..N).map(|k| at[k].1).sum::<f64>()) / width;
    ((simpson - trapezoid).abs() <= allowed(simpson, trapezoid) / 2.0).then_some((simpson, central))
}

/// The allowance: 1% of the larger, or 1e-4.
pub(crate) fn allowed(a: f64, b: f64) -> f64 {
    (0.01 * a.abs().max(b.abs())).max(1e-4)
}

/// The parameter gradient of the distance at sample points against central differences
/// through `set` ([`against_central`]): within 1% of the larger, or 1e-4. Every literal at
/// every point; each literal the distance reads must change it somewhere.
#[test]
fn parameter_gradients_agree_with_central_differences() {
    let dir = built("lift-gradients", true);
    let mut host = CpuHost::load(&dir).expect("load");
    let n = one_u32(&mut host, "literals", &[]);
    let reads = one_u32(&mut host, "reads_count", &[]);
    // Every literal but the sample box's six (`AROUND`), which only `sample` reads.
    assert_eq!(reads, n - 6, "the literals the blob's distance reads");
    let points = 256;
    let (mut worst, mut checked, mut kinks) = (0.0f64, 0, 0);
    let mut nonzero = vec![0u32; n as usize];
    for k in 0..points {
        let p = sample(&mut host, k);
        let d = distance(&mut host, p);
        for i in 0..n {
            let v = one_f32(&mut host, "value", &[Value::I32(i as i32)]);
            nonzero[i as usize] += u32::from(derived(&mut host, p, i) != 0.0);
            let Some((derived, central)) = against_central(&mut host, p, i, v, d) else {
                kinks += 1;
                continue;
            };
            let err = (derived - central).abs();
            assert!(
                err <= allowed(derived, central),
                "literal {i} at {p:?}: derived {derived}, central differences {central} (error {err:.3e})"
            );
            worst = worst.max(err / allowed(derived, central));
            checked += 1;
        }
    }
    let never: Vec<usize> = (0..reads as usize).filter(|&i| nonzero[i] == 0).collect();
    eprintln!(
        "{points} points × {n} literals: {checked} checked, {} non-zero, {kinks} with a kink in \
         the step; worst {worst:.3} of the allowance; never non-zero: {never:?}",
        nonzero.iter().sum::<u32>()
    );
    assert!(kinks * 100 <= checked, "{kinks} of {checked} had a kink");
    assert!(never.is_empty(), "literals whose derivative is 0 at every point: {never:?}");
}

#[test]
fn several_literals_derived_at_once_each_get_their_own() {
    let dir = built("lift-gradients4", true);
    let mut host = CpuHost::load(&dir).expect("load");
    let n = one_u32(&mut host, "literals", &[]);
    for k in 0..32 {
        let p = sample(&mut host, k);
        let i = (k * 7) % (n - 3);
        let at =
            |i: u32| [Value::F32(p[0]), Value::F32(p[1]), Value::F32(p[2]), Value::I32(i as i32)];
        let four = host.call_export("derivatives4", &at(i)).expect("derivatives4");
        for (j, d) in four.iter().enumerate() {
            let one = one_f32(&mut host, "derivative", &at(i + j as u32));
            assert_eq!(*d, Value::F32(one), "literal {} at {p:?}", i + j as u32);
        }
    }
}

#[test]
fn a_normal_build_has_no_literals_and_no_derivatives() {
    let mut host = CpuHost::load(built("lift-none", false)).expect("load");
    assert_eq!(one_u32(&mut host, "literals", &[]), 0);
    assert_eq!(one_u32(&mut host, "reads_count", &[]), 0);
    let d = one_f32(
        &mut host,
        "derivative",
        &[Value::F32(0.0), Value::F32(0.5), Value::F32(0.0), Value::I32(3)],
    );
    assert_eq!(d, 0.0);
}

/// An edit of each literal of a lifted build's report (`lift.json`) to the value it has.
pub(crate) fn same_values(r: &serde_json::Value) -> Vec<wrela_driver::edit::LiteralEdit> {
    let files = r["files"].as_array().unwrap();
    r["literals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            let file = &files[l["file"].as_u64().unwrap() as usize];
            wrela_driver::edit::LiteralEdit {
                file: file["path"].as_str().unwrap().into(),
                start: l["start"].as_u64().unwrap() as u32,
                end: l["end"].as_u64().unwrap() as u32,
                text: l["text"].as_str().unwrap().to_string(),
                hash: file["hash"].as_str().unwrap().into(),
                value: l["value"].as_f64().unwrap() as f32,
            }
        })
        .collect()
}

/// `wrela edit` with every literal's own value plans no change; a new value, written to a copy
/// of the package, is what the rebuilt program reads.
#[test]
fn edits_round_trip_through_the_source() {
    use wrela_driver::edit::{plan, write};
    let dir = built("lift-edit", true);
    let r = report(&dir);
    let lits = r["literals"].as_array().unwrap();
    let edits = same_values(&r);
    let planned = plan(&package(), &edits).expect("the same values plan");
    for f in &planned {
        assert_eq!(f.old_hash, f.new_hash, "the same values change nothing: {:?}", f.lines);
    }

    // A copy of the package, with R made 0.2.
    let copy = super::scratch("lift-edit-copy");
    wrela_tests::copy_dir(&package(), &copy);
    let i = lits.iter().position(|l| l["text"] == "0.11").unwrap();
    let mut e = edits[i].clone();
    e.value = 0.2;
    let planned = plan(&copy, &[e]).expect("plan");
    assert_eq!(planned[0].lines.len(), 1, "{:?}", planned[0].lines);
    assert_eq!(planned[0].lines[0].2.trim(), "const R: f32 = 0.20");
    write(&copy, &planned).expect("write");
    // The old hash is refused now.
    let mut stale = edits[i].clone();
    stale.value = 0.3;
    assert!(plan(&copy, &[stale]).is_err(), "an edit of a changed file is refused");
    let out = wrela_driver::build_lifted(&copy, &["shapes".into()], false).unwrap();
    assert!(!out.has_errors());
    let rebuilt = super::scratch("lift-edit-rebuilt");
    out.write_to(&rebuilt).unwrap();
    let mut host = CpuHost::load(&rebuilt).unwrap();
    assert_eq!(one_f32(&mut host, "value", &[Value::I32(i as i32)]), 0.2);
}

/// The GPU frame of a lifted build is the normal build's; a literal set between frames shows
/// in the next one.
#[test]
#[ignore = "needs a GPU"]
fn the_gpu_draws_the_same_and_reads_a_change_next_frame() {
    let (w, h) = (256, 256);
    let times: Vec<f32> = (0..2).map(|i| frame_time(i, 60.0)).collect();
    let normal =
        Host::load(built("lift-gpu-normal", false)).unwrap().run_frames(&times, w, h).unwrap();
    let dir = built("lift-gpu-lifted", true);
    let r = report(&dir);
    let mut host = Host::load(&dir).unwrap();
    let lifted = host.run_frames(&times, w, h).unwrap();
    let diff = image::compare(&normal.frame, &lifted.frame).unwrap();
    eprintln!("lifted against normal: mean {:.4}/255, max {}/255", diff.mean, diff.max);
    assert!(diff.mean <= 0.5, "mean {:.4}", diff.mean);
    assert_eq!(normal.frame, lifted.frame, "the same pixels");

    let i = r["literals"].as_array().unwrap().iter().position(|l| l["text"] == "0.11").unwrap();
    host.call_export("set", &[Value::I32(i as i32), Value::F32(0.25)]).unwrap();
    host.frame(times[1], w, h).unwrap();
    let changed = host.read_screen().unwrap();
    let diff = image::compare(&lifted.frame, &changed).unwrap();
    eprintln!("after the change: {} of {} channels differ", diff.differing, diff.channels);
    assert!(diff.differing > 200, "the bigger head shows in the next frame");
}
