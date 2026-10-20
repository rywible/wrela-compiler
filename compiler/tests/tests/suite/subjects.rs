//! AC7: the authoring test subjects, round 1's wolf and grazer (design-archive-2026-10:
//! experiments/agent-authoring/), ported to the engine's creature API (content/wolf,
//! content/grazer). Each port matches its WGSL original (compiler/tests/fixtures/round1/,
//! with round 1's helper library) within 0.5 mm at 128 points of `Box3::sample` near the
//! surface; the original runs on the GPU, as round 1's tool ran it. The ports are their frozen
//! copies (compiler/tests/frozen/wolf, grazer, on the frozen engine): these check the compiler,
//! its builds and lifted builds of them, so their inputs don't change with content/.

use std::path::PathBuf;
use wrela_host::{CpuHost, Value};
use wrela_tests::{Bind, RawGpu, bytes_of, f32s, one_f32, one_vec3, repo_root};

/// The largest difference allowed between a port and its original, in metres.
const LIMIT: f64 = 0.0005;
/// How many points, and how near the surface (by the port's distance) each must be.
const POINTS: usize = 128;
const NEAR: f32 = 0.02;

/// The subject `name`'s build (its frozen copy, compiler/tests/frozen/<name>), shared by the
/// suites.
fn built(name: &str) -> PathBuf {
    super::built(&format!("frozen/{name}"))
}

/// The subject `name`'s frozen copy.
fn frozen(name: &str) -> PathBuf {
    repo_root().join("compiler/tests/frozen").join(name)
}

/// The first `n` of the subject's samples (`sample(seed, i)` in its box) within `NEAR` of its
/// surface, and the port's distance at each.
fn near_points(host: &mut CpuHost, n: usize) -> (Vec<[f32; 3]>, Vec<f32>) {
    let (mut points, mut ours) = (Vec::new(), Vec::new());
    let mut i = 0;
    while points.len() < n {
        let p = one_vec3(host, "sample", &[Value::I32(1), Value::I32(i)]);
        let d = one_f32(host, "distance", &[Value::F32(p[0]), Value::F32(p[1]), Value::F32(p[2])]);
        if d.abs() < NEAR {
            points.push(p);
            ours.push(d);
        }
        i += 1;
        assert!(i < 4_000_000, "too few points near the surface");
    }
    (points, ours)
}

/// Round 1's field at each point, on the GPU: its helper library, the creature's file, and a
/// kernel that writes `field(p)`.
pub(crate) fn originals(name: &str, points: &[[f32; 3]]) -> Vec<f32> {
    let dir = repo_root().join("compiler/tests/fixtures/round1");
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).expect(f);
    let wgsl = format!(
        "{}\n{}\n{}",
        read("lib.wgsl"),
        read(&format!("{name}.wgsl")),
        "@group(0) @binding(0) var<storage, read> points: array<vec4<f32>>;\n\
         @group(0) @binding(1) var<storage, read_write> out: array<f32>;\n\
         @compute @workgroup_size(64)\n\
         fn main(@builtin(global_invocation_id) id: vec3<u32>) {\n\
             out[id.x] = field(points[id.x].xyz);\n\
         }\n"
    );
    let flat: Vec<f32> = points.iter().flat_map(|p| [p[0], p[1], p[2], 0.0]).collect();
    let gpu = RawGpu::new().expect("a GPU");
    let n = points.len() as u64;
    let (_, out) = gpu.run(
        &wgsl,
        "main",
        &[Bind::Read(&bytes_of(&flat)), Bind::Write(n * 4)],
        (n / 64) as u32,
        1,
    );
    f32s(&out[0])
}

fn matches_its_original(name: &str) {
    let mut host = CpuHost::load(built(name)).expect("load");
    let (points, ours) = near_points(&mut host, POINTS);
    let theirs = originals(name, &points);
    let mut worst = (0.0f64, 0);
    let mut sum = 0.0;
    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        let gap = (f64::from(*a) - f64::from(*b)).abs();
        assert!(gap.is_finite(), "{name}: at {:?}, ours {a}, theirs {b}", points[i]);
        sum += gap;
        if gap > worst.0 {
            worst = (gap, i);
        }
    }
    let (gap, i) = worst;
    eprintln!(
        "{name}: {POINTS} points within {NEAR} m of the surface: largest gap {:.3e} m at {:?} \
         (ours {}, theirs {}), mean {:.3e} m",
        gap,
        points[i],
        ours[i],
        theirs[i],
        sum / POINTS as f64
    );
    assert!(gap <= LIMIT, "{name}: the port differs from round 1's by {gap} m at {:?}", points[i]);
}

#[test]
#[ignore = "needs a GPU"]
fn the_wolf_matches_round_1s() {
    matches_its_original("wolf");
}

#[test]
#[ignore = "needs the runtime"]
fn the_grazer_matches_round_1s() {
    matches_its_original("grazer");
}
// ---- AC4 and AC5 on the subjects: lifted builds and parameter gradients --------------------------

/// The subject `name`'s build with its package's literals lifted, shared as [`built`]'s is.
fn lifted(name: &str) -> PathBuf {
    super::built_lifted(&format!("frozen/{name}"), &[name])
}

/// Every float literal of the subject's package is lifted, or its report says why; `wrela
/// edit` with each one's own value changes nothing.
fn lifts_every_literal(name: &str) {
    use wrela_syntax::token::TokenKind;
    let dir = lifted(name);
    let r = super::lift::report(&dir);
    let lifted = r["literals"].as_array().unwrap();
    let not = r["not_lifted"].as_array().unwrap();
    for n in not {
        assert!(!n["why"].as_str().unwrap().is_empty(), "{n}");
    }
    let mut floats = 0;
    for f in r["files"].as_array().unwrap() {
        let path = frozen(name).join(f["path"].as_str().unwrap());
        let text = std::fs::read_to_string(path).unwrap();
        floats += wrela_syntax::lexer::lex(wrela_diag::FileId(0), &text)
            .tokens
            .iter()
            .filter(|t| matches!(t.kind, TokenKind::Float | TokenKind::Suffixed))
            .count();
    }
    let lifted_floats = lifted
        .iter()
        .filter(|l| {
            let t = l["text"].as_str().unwrap();
            t.contains('.') || t.contains('e') || t.ends_with('m')
        })
        .count();
    eprintln!(
        "{name}: {} literals lifted ({lifted_floats} written as floats), {} not",
        lifted.len(),
        not.len()
    );
    assert_eq!(
        lifted_floats + not.len(),
        floats,
        "{name}: each float literal is in the report once"
    );
    let planned = wrela_driver::edit::plan(&frozen(name), &super::lift::same_values(&r))
        .expect("the same values plan");
    for f in &planned {
        assert_eq!(
            f.old_hash, f.new_hash,
            "{name}: the same values change {}: {:?}",
            f.file, f.lines
        );
    }
}

/// The lifted build's distances are the normal build's, bit for bit, at the points near the
/// surface.
fn computes_what_the_normal_build_does(name: &str) {
    let mut normal = CpuHost::load(built(name)).expect("load");
    let mut lifted = CpuHost::load(lifted(name)).expect("load");
    let (points, ours) = near_points(&mut normal, POINTS);
    for (p, a) in points.iter().zip(&ours) {
        let b = super::lift::distance(&mut lifted, *p);
        assert_eq!(a.to_bits(), b.to_bits(), "{name} at {p:?}: normal {a}, lifted {b}");
    }
}

/// At 256 points of `Box3::sample` near the subject's surface (`near_points`), `k` of the
/// literals its distance reads each (in turn, so each is checked at some points): the derived
/// derivative against central differences, as the lift suite checks them
/// (`lift::against_central`), within 1% or 1e-4.
fn gradients_agree(name: &str) {
    let dir = lifted(name);
    let mut host = CpuHost::load(&dir).expect("load");
    let n = wrela_tests::one_u32(&mut host, "literals", &[]);
    let reads = wrela_tests::one_u32(&mut host, "reads_count", &[]);
    let read: Vec<u32> = (0..reads)
        .map(|k| wrela_tests::one_u32(&mut host, "read_literal", &[Value::I32(k as i32)]))
        .collect();
    eprintln!("{name}: the distance reads {reads} of {n} literals");
    let (points, _) = near_points(&mut host, 256);
    let per_point = 4;
    let (mut checked, mut kinks, mut nonzero, mut worst) = (0, 0, 0, 0.0f64);
    for (k, &p) in points.iter().enumerate() {
        let d = super::lift::distance(&mut host, p);
        for j in 0..per_point {
            let i = read[(k * per_point as usize + j as usize) % read.len()];
            let v = one_f32(&mut host, "value", &[Value::I32(i as i32)]);
            let Some((derived, central)) = super::lift::against_central(&mut host, p, i, v, d)
            else {
                kinks += 1;
                continue;
            };
            let err = (derived - central).abs();
            let allowed = super::lift::allowed(derived, central);
            assert!(
                err <= allowed,
                "{name}: literal {i} at {p:?}: derived {derived}, central {central}"
            );
            worst = worst.max(err / allowed);
            checked += 1;
            nonzero += usize::from(derived != 0.0);
        }
    }
    eprintln!(
        "{name}: 256 points near the surface × {per_point} literals: {checked} checked, {nonzero} non-zero, {kinks} with a kink in the step; worst {worst:.3} of the allowance"
    );
    assert!(kinks * 50 <= checked, "{name}: {kinks} of {checked} had a kink");
    assert!(
        nonzero * 20 >= checked,
        "{name}: only {nonzero} of {checked} derivatives are non-zero"
    );
}

#[test]
fn the_wolfs_literals_are_lifted_or_have_reasons() {
    lifts_every_literal("wolf");
}

#[test]
fn the_grazers_literals_are_lifted_or_have_reasons() {
    lifts_every_literal("grazer");
}

#[test]
fn a_lifted_wolf_computes_what_the_wolf_does() {
    computes_what_the_normal_build_does("wolf");
}

#[test]
fn a_lifted_grazer_computes_what_the_grazer_does() {
    computes_what_the_normal_build_does("grazer");
}

#[test]
fn the_wolfs_parameter_gradients_agree_with_central_differences() {
    gradients_agree("wolf");
}

#[test]
fn the_grazers_parameter_gradients_agree_with_central_differences() {
    gradients_agree("grazer");
}
