//! AC7: the lens's test subjects, round 1's wolf and grazer (design-archive-2026-10:
//! experiments/agent-authoring/), ported to the engine's creature API (examples/wolf,
//! examples/grazer). Each port matches its WGSL original (compiler/tests/fixtures/round1/,
//! with round 1's helper library) within 0.5 mm at 4,096 points of `Box3::sample` near the
//! surface; the original runs on the GPU, as round 1's tool ran it.

use std::path::PathBuf;
use wrela_host::{CpuHost, Value};
use wrela_tests::{Bind, RawGpu, bytes_of, f32s, one_f32, one_vec3, repo_root};

/// The largest difference allowed between a port and its original, in metres.
const LIMIT: f64 = 0.0005;
/// How many points, and how near the surface (by the port's distance) each must be.
const POINTS: usize = 4096;
const NEAR: f32 = 0.02;

/// The subject `name`'s build (examples/<name>), shared by the suites.
fn built(name: &str) -> PathBuf {
    super::built(&format!("../../examples/{name}"))
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
#[ignore = "long: needs a GPU"]
fn the_grazer_matches_round_1s() {
    matches_its_original("grazer");
}

/// The mesh the engine's realization makes of a subject (its program realizes it in `init`):
/// the vertices' rest positions and the quads, read back after a frame. After `init` the
/// program holds the mesh's buffers first, oldest first: the vertices' count and the vertices
/// (an append buffer makes its count first), the quads' count and the quads, the draw's and the
/// skin pass's arguments, the holes and the tally; then the palette's.
fn mesh(host: &mut wrela_host::Host) -> (Vec<[f32; 3]>, Vec<[u32; 6]>) {
    let held = host.buffers();
    let [vcount, verts, qcount, quads, _draw, _skin, _holes, _tally, _palettes] = held[..] else {
        panic!("the program holds {} buffers, not the mesh's eight and a palette", held.len())
    };
    let count = |h: u32, host: &mut wrela_host::Host| {
        wrela_tests::u32s(&host.read_buffer(h).expect("a count"))[0] as usize
    };
    let (nv, nq) = (count(vcount, host), count(qcount, host));
    let vbytes = host.read_buffer(verts).expect("vertices");
    let qwords = wrela_tests::u32s(&host.read_buffer(quads).expect("quads"));
    // A `SkinVertex` is 48 bytes: its rest position first. A quad is six indices.
    assert!(nv * 48 <= vbytes.len(), "{nv} vertices overflow the buffer");
    assert!(nq * 6 <= qwords.len(), "{nq} quads overflow the buffer");
    let q = (0..nq).map(|i| std::array::from_fn(|k| qwords[6 * i + k]));
    (wrela_tests::positions(&vbytes, nv), q.collect())
}

/// The pieces a mesh is in: its vertices joined by its quads (union-find).
fn pieces(verts: usize, quads: &[[u32; 6]]) -> (usize, Vec<usize>) {
    let mut parent: Vec<usize> = (0..verts).collect();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    let mut used = vec![false; verts];
    for q in quads {
        for &v in q {
            used[v as usize] = true;
        }
        for k in 1..6 {
            let (a, b) = (root(&mut parent, q[0] as usize), root(&mut parent, q[k] as usize));
            parent[a] = b;
        }
    }
    let mut sizes = std::collections::BTreeMap::new();
    for (i, _) in used.iter().enumerate().filter(|(_, u)| **u) {
        *sizes.entry(root(&mut parent, i)).or_insert(0) += 1;
    }
    let mut s: Vec<usize> = sizes.into_values().collect();
    s.sort_unstable_by(|a, b| b.cmp(a));
    (s.len(), s)
}

/// Sketch 02's extraction realizes the subject in one piece, and a frame of it walking draws.
fn realized_in_one_piece(name: &str) {
    use wrela_host::{Host, frame_time};
    let dir = built(name);
    let mut host = Host::load(&dir).expect("load");
    let times: Vec<f32> = (0..30).map(|i| frame_time(i, 60.0)).collect();
    let run = host.run_frames(&times, 640, 400).expect("frames");
    let png = repo_root().join(format!("target/tmp/subject-{name}.png"));
    std::fs::create_dir_all(png.parent().unwrap()).expect("target/tmp");
    run.write_png(&png).expect("png");
    let (verts, quads) = mesh(&mut host);
    let (n, sizes) = pieces(verts.len(), &quads);
    eprintln!(
        "{name}: {} vertices, {} quads, {n} piece(s) (largest {:?}); a frame in {}",
        verts.len(),
        quads.len(),
        &sizes[..sizes.len().min(5)],
        png.display()
    );
    assert!(quads.len() > 1000, "{name}: only {} quads", quads.len());
    assert_eq!(n, 1, "{name} is in {n} pieces: {sizes:?}");
    // The sky is (0.55, 0.7, 0.85): the creature covers some of the frame.
    let sky = [140u8, 178, 217];
    let covered =
        run.frame.chunks_exact(4).filter(|p| (0..3).any(|c| p[c].abs_diff(sky[c]) > 8)).count();
    let share = covered as f64 / (640.0 * 400.0);
    assert!(share > 0.03 && share < 0.8, "{name} covers {:.1}% of the frame", 100.0 * share);
}

#[test]
#[ignore = "long: needs a GPU"]
fn the_wolf_is_realized_in_one_piece() {
    realized_in_one_piece("wolf");
}

#[test]
#[ignore = "long: needs a GPU"]
fn the_grazer_is_realized_in_one_piece() {
    realized_in_one_piece("grazer");
}

// ---- AC4 and AC5 on the subjects: lifted builds and parameter gradients --------------------------

/// The subject `name`'s build with its package's literals lifted, shared as [`built`]'s is.
fn lifted(name: &str) -> PathBuf {
    super::built_lifted(&format!("../../examples/{name}"), &[name])
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
        let path = repo_root().join("examples").join(name).join(f["path"].as_str().unwrap());
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
    let planned = wrela_driver::edit::plan(
        &repo_root().join("examples").join(name),
        &super::lift::same_values(&r),
    )
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
    let per_point = wrela_tests::sized(4, 16);
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

/// A skeleton holds `MAX_BONES` (64) bones, enough for round 1's grazer brief with a withers,
/// a jaw and shoulder blades (two round 4 authors dropped joints to fit 32); one more fails
/// with the limit named.
#[test]
fn a_skeleton_holds_64_bones_and_says_so_past_them() {
    let dir = super::engine_package(
        "subject-skeleton-limit",
        "bones",
        "use engine::creature::{MAX_BONES, Skeleton}

fn grown(n: u32) -> Skeleton {
    var s = Skeleton::new()
    var last = s.root(\"root\", vec3(0.0, 1.0, 0.0))
    for _ in 1..n {
        last = s.bone(\"joint\", last, vec3(0.0, 0.0, 0.01))
    }
    s
}

@test
fn holds_its_limit() {
    assert(MAX_BONES == 64 && grown(64).len() == 64)
}

@test
fn one_past_it_fails() {
    let s = grown(65)
}

pub fn frame(time: f32, width: u32, height: u32) {}
",
    );
    let out = wrela_driver::test(&dir, None);
    let errors: Vec<_> = out.diagnostics.iter().filter(|d| d.is_error()).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let ran: Vec<(&str, Option<String>)> = out
        .results
        .iter()
        .map(|r| (r.name.as_str(), r.failure.as_ref().map(|d| d.message.clone())))
        .collect();
    assert_eq!(ran[0], ("holds_its_limit", None));
    let past = ran[1].1.as_deref().unwrap_or("");
    assert!(past.contains("a skeleton has at most 64 bones"), "{past}");
}

/// The anatomy kit's canine (engine::anatomy) with a wolf's proportions: its withers 0.8 m up
/// and its nose where the reference photo's is, its paws on the ground and not below it, and
/// its gradient near 1 where the renderer steps (its blocks are exact distances, blended).
#[test]
fn the_anatomy_kits_wolf_stands_as_the_photo_does() {
    let dir = super::engine_package(
        "subject-anatomy-wolf",
        "kit",
        "use engine::anatomy::{Coat, canine, canine_skeleton, wolf}
use engine::creature::Creature
use std::derive::Box3
use std::field::{Color, Surface, bottom, front, rgb, top}

fn grey() -> Color {
    rgb(r: 0.5, g: 0.5, b: 0.5)
}

fn coat() -> Coat<Color> {
    Coat {
        back: grey(),
        saddle: grey(),
        belly: grey(),
        legs: grey(),
        muzzle: grey(),
        ears: grey(),
        eyes: grey(),
        nose: grey(),
        tail_tip: grey(),
    }
}

@test
fn stands_at_its_height() {
    let f = canine(wolf(), coat()).field()
    let withers = top(f, x: 0.0, z: 0.07)
    assert(abs(withers - 0.8) < 0.04)
    let nose = front(f, x: 0.0, y: 0.72)
    assert(nose > 0.5 && nose < 0.62)
}

@test
fn its_paws_are_on_the_ground() {
    let f = canine(wolf(), coat()).field()
    let s = canine_skeleton(wolf())
    for name in [\"fore paw left\", \"hind paw left\"] {
        let p = s.rest(s.find(name))
        let sole = bottom(f, x: p.x, z: p.z)
        assert(abs(sole) < 0.002)
    }
}

@test
fn its_gradient_stays_near_1() {
    let f = canine(wolf(), coat()).field()
    let around = Box3 { lo: vec3(-0.25, -0.02, -0.75), hi: vec3(0.25, 1.05, 0.7) }
    var worst = 0.0
    var near = 0
    for i in 0..6000 {
        let p = around.sample(3, i)
        let (d, g) = f.sample(p)
        if abs(d) < 0.01 {
            worst = max(worst, length(g))
            near += 1
        }
    }
    assert(near > 100 && worst < 1.25)
}

pub fn frame(time: f32, width: u32, height: u32) {}
",
    );
    assert_eq!(super::tests_pass(&dir), 3);
}
