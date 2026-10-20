//! The herd (examples/herd) against spike 01 (#42 AC1–AC7). The fixtures are the spike's files,
//! unchanged (compiler/tests/fixtures/spike01): its CPU module (`cpu.wasm`), its kernels and its
//! seed-1 parameters.
//!
//! CPU comparisons run both sides in one harness, the native host's wasmtime, alternating after a
//! warm-up round (M1's method): the spike's `cpu.wasm` and the herd's build.

use crate::built;
use std::path::{Path, PathBuf};
use wrela_host::{CpuBuild, CpuHost, Host, Value};
use wrela_tests::spike01::{HARNESS_D, fixture, fixture_path, harness};
use wrela_tests::{files_under, repo_root};

/// The herd, built once for the suite (`built` builds it under the suite's scratch).
fn herd() -> PathBuf {
    built("../../examples/herd")
}

fn ours() -> CpuHost {
    CpuBuild::load(herd()).expect("load the herd").start_with(1).expect("start")
}

// ---- AC2: realization against the spike's extraction ------------------------------------------

/// A mesh: its vertices and its triangles (three indices each).
struct Tris {
    verts: Vec<[f32; 3]>,
    tris: Vec<[u32; 3]>,
}

impl Tris {
    fn new(verts: Vec<[f32; 3]>, indices: &[u32]) -> Tris {
        let tris = indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
        Tris { verts, tris }
    }

    fn corners(&self, t: [u32; 3]) -> [[f32; 3]; 3] {
        t.map(|i| self.verts[i as usize])
    }
}
/// Our realization of one grazer: its counts and mesh.
struct Ours {
    live: u32,
    tris: u32,
    holes: u32,
    flags: u32,
    mesh: Tris,
}

/// A mesh's buffers, in the order `Mesh::new` makes them: the vertices' count and items, the
/// quads' count and items, the draw's arguments, the skin pass's, the holes and the tally.
const MESH_BUFFERS: usize = 8;

/// A mesh's counts and geometry from its buffers (`MESH_BUFFERS`). The draw's arguments begin
/// with its index count; the tally holds the vertices, holes, flags, vertices and quads pushed,
/// and live blocks.
fn read_mesh(host: &mut wrela_host::Host, mesh: &[u32]) -> Ours {
    let draw = wrela_tests::u32s(&host.read_buffer(mesh[4]).expect("the draw's arguments"));
    let tally = wrela_tests::u32s(&host.read_buffer(mesh[7]).expect("the tally"));
    let (nv, ni) = (tally[0] as usize, draw[0] as usize);
    let verts = wrela_tests::positions(&host.read_buffer(mesh[1]).expect("vertices"), nv);
    let indices = wrela_tests::u32s(&host.read_buffer(mesh[3]).expect("quads"));
    Ours {
        live: tally[5],
        tris: draw[0] / 3,
        holes: tally[1],
        flags: tally[2],
        mesh: Tris::new(verts, &indices[..ni]),
    }
}

/// A `Realizer` spreads a level change over frames (`record_vertices` a slice at a time): the
/// mesh is the one a whole realization makes, triangle for triangle, at 3 and 1.5 cm, in 1, 5
/// and 40 slices (40: most are past the live blocks' end, and empty).
#[test]
#[ignore = "needs a GPU"]
fn a_realization_in_slices_is_the_whole_ones() {
    let dir = crate::built("herd-realize");
    let mut host = wrela_host::Host::load(&dir).expect("load");
    let newest = |host: &mut wrela_host::Host| {
        let b = host.buffers();
        b[b.len() - MESH_BUFFERS..].to_vec()
    };
    // Each triangle as its corners' bits, from its lowest corner on (so the vertices' order in
    // the buffer, which slices change, doesn't matter, but the winding does).
    let triangles = |o: &Ours| {
        let mut t: Vec<[[u32; 3]; 3]> = o
            .mesh
            .tris
            .iter()
            .map(|&t| {
                let c = o.mesh.corners(t).map(|p| p.map(f32::to_bits));
                let k = (0..3).min_by_key(|&k| c[k]).expect("a corner");
                [c[k], c[(k + 1) % 3], c[(k + 2) % 3]]
            })
            .collect();
        t.sort();
        t
    };
    for cell in [0.03f32, 0.015] {
        host.call_export("realize", &[Value::I32(1), Value::F32(cell)]).expect("realize");
        let five = newest(&mut host);
        let whole = read_mesh(&mut host, &five);
        let want = triangles(&whole);
        assert!(whole.tris > 1000 && whole.holes == 0 && whole.flags == 0);
        for slices in [1, 5, 40] {
            host.call_export(
                "realize_sliced",
                &[Value::I32(1), Value::F32(cell), Value::I32(slices)],
            )
            .expect("realize_sliced");
            let five = newest(&mut host);
            let sliced = read_mesh(&mut host, &five);
            let what = format!("{cell} m in {slices} slices");
            assert_eq!(
                (sliced.live, sliced.tris, sliced.holes, sliced.flags),
                (whole.live, whole.tris, whole.holes, whole.flags),
                "{what}"
            );
            assert!(triangles(&sliced) == want, "{what}: the triangles differ");
        }
        eprintln!("{cell} m: {} triangles, the same in 1, 5 and 40 slices", whole.tris);
    }
}

/// More live blocks than WebGPU dispatches workgroups along x (65,535): herd-realize's slab,
/// 750 × 750 × 3 cells, about 70,000 live blocks. Its passes go in chunks, so its mesh is whole:
/// a quad for each lattice edge its faces cross (2 × 750² + 4 × 750 × 3), no holes, and the same
/// triangles in 2 slices (the second past 65,535 blocks on its own). It can't be smaller.
#[test]
#[ignore = "needs the runtime"]
fn a_realization_past_65535_live_blocks_is_whole() {
    let mut host = wrela_host::Host::load(crate::built("herd-realize")).expect("load");
    let realize = |host: &mut wrela_host::Host, slices: i32| {
        host.call_export("realize_slab", &[Value::I32(slices)]).expect("realize_slab");
        let b = host.buffers();
        let mesh = b[b.len() - MESH_BUFFERS..].to_vec();
        read_mesh(host, &mesh)
    };
    let whole = realize(&mut host, 0);
    let quads = 2 * 750 * 750 + 4 * 750 * 3;
    assert!(whole.live > 65_535, "only {} live blocks", whole.live);
    assert_eq!((whole.tris, whole.holes, whole.flags), (2 * quads, 0, 0));
    let corners = |o: &Ours| {
        let mut t: Vec<[[u32; 3]; 3]> = o
            .mesh
            .tris
            .iter()
            .map(|&t| {
                let c = o.mesh.corners(t).map(|p| p.map(f32::to_bits));
                let k = (0..3).min_by_key(|&k| c[k]).expect("a corner");
                [c[k], c[(k + 1) % 3], c[(k + 2) % 3]]
            })
            .collect();
        t.sort();
        t
    };
    let sliced = realize(&mut host, 2);
    assert_eq!(
        (sliced.live, sliced.tris, sliced.holes, sliced.flags),
        (whole.live, whole.tris, whole.holes, whole.flags)
    );
    assert!(corners(&sliced) == corners(&whole), "the triangles differ in 2 slices");
    eprintln!("{} live blocks, {} triangles", whole.live, whole.tris);
}
// ---- AC7: the herd's grazer against field.wgsl ------------------------------------------------

/// The herd's grazer, seed 1, at the `n` points herd-realize's export `export` (`evaluate` or
/// `near_surface`) samples, the newest three buffers it holds: the points (vec4s, as bytes), its
/// distances and its samples (vec4s: the distance, then the gradient or the channels); and the
/// command batches it recorded, if `record`.
fn grazer_points(
    export: &str,
    n: u32,
    record: bool,
) -> (Vec<u8>, Vec<f32>, Vec<f32>, Vec<Vec<u8>>) {
    use wrela_tests::f32s;
    let options = wrela_host::Options { record, ..wrela_host::Options::default() };
    let mut host = Host::load_with(built("herd-realize"), &options).expect("load");
    host.call_export(export, &[Value::I32(1), Value::I32(n as i32)]).expect(export);
    let made = host.buffers();
    let [.., p, d, s] = made[..] else { panic!("{export} made {made:?}") };
    let points = host.read_buffer(p).expect("points");
    let dist = f32s(&host.read_buffer(d).expect("distances"));
    let samp = f32s(&host.read_buffer(s).expect("samples"));
    (points, dist, samp, host.take_batches())
}
// ---- AC7: the herd's grazer against the spike's ---------------------------------------------------

/// AC7: at every vertex of the spike's 1.5 cm mesh of seed 1, the herd's skin weights (the
/// creature API's skin bindings, `Parts::skin_at` under the vertex's mask, a cell wide, falling
/// off over 6 cm) pick the same four bones, each weight within 1/255.
#[test]
#[ignore = "needs a GPU and bun"]
fn the_herds_skin_weights_are_the_spikes() {
    let (vertices, cell) = {
        let spike = wrela_tests::spike01::Spike::new();
        let (_, inds) = spike.extract_herd(0.015, 1);
        (spike.vertices(&inds[0]), 0.015f32)
    };
    let mut host = ours();
    let (mut bones_off, mut weight_off, mut worst) = (0, 0, 0u8);
    for v in &vertices {
        let [x, y, z] = [0, 1, 2].map(|k| f32::from_bits(v[k]));
        let r = host
            .call_export(
                "skin_at",
                &[
                    Value::I32(1),
                    Value::F32(x),
                    Value::F32(y),
                    Value::F32(z),
                    Value::I32(v[8] as i32),
                    Value::I32(0),
                    Value::F32(cell),
                ],
            )
            .expect("skin_at");
        let [Value::I32(b), Value::I32(w)] = r[..] else { panic!("{r:?}") };
        let (ours_b, ours_w) = (b.to_le_bytes(), w.to_le_bytes());
        let (theirs_b, theirs_w) = (v[3].to_le_bytes(), v[7].to_le_bytes());
        // The same bones with the same weights, whatever their order, where weights are equal.
        let mut a: Vec<(u8, u8)> =
            (0..4).filter(|&k| ours_w[k] > 0).map(|k| (ours_b[k], ours_w[k])).collect();
        let mut t: Vec<(u8, u8)> =
            (0..4).filter(|&k| theirs_w[k] > 0).map(|k| (theirs_b[k], theirs_w[k])).collect();
        a.sort();
        t.sort();
        let same_bones = a.len() == t.len() && a.iter().zip(&t).all(|(p, q)| p.0 == q.0);
        if !same_bones {
            bones_off += 1;
            continue;
        }
        let off = a.iter().zip(&t).map(|(p, q)| p.1.abs_diff(q.1)).max().unwrap_or(0);
        worst = worst.max(off);
        weight_off += usize::from(off > 1);
    }
    eprintln!(
        "{} vertices: {bones_off} with other bones, {weight_off} with a weight over 1/255 off (the most {worst}/255)",
        vertices.len()
    );
    assert!(vertices.len() > 10_000, "only {} vertices", vertices.len());
    assert_eq!(bones_off, 0, "vertices whose bones differ");
    assert_eq!(weight_off, 0, "vertices whose weights differ by more than 1/255");
}

/// AC7: the herd's grazer, seed 1, matches field.wgsl with the seed-1 parameters at 4,096 points
/// from `Box3::sample` within 2 cm of its surface, on the GPU: distances within 1e-6 m, channels
/// (torso-ness, hoof-ness) within 1e-5.
#[test]
#[ignore = "needs a GPU"]
fn the_herds_grazer_is_field_wgsls_near_the_surface() {
    use wrela_tests::{Bind, RawGpu, f32s};
    let n: u32 = 4096;
    let (points, dist, samp, _) = grazer_points("near_surface", n, false);
    let field = fixture("field.wgsl");
    let params = std::fs::read(fixture_path("params-seed1.f32")).expect("params-seed1.f32");
    let gpu = RawGpu::new().expect("a GPU");
    let run = |wgsl: String, entry: &str, out: u64| {
        let (_, o) = gpu.run(
            &wgsl,
            entry,
            &[Bind::Uniform(&params), Bind::Read(&points), Bind::Write(out)],
            n / 64,
            1,
        );
        f32s(&o[0])
    };
    let hd = run(format!("{field}{HARNESS_D}"), "eval_d", u64::from(n) * 4);
    let hand_c = format!("{field}{}", harness("eval_c", "vec4f(s.d, s.ch, 0.0)"));
    let hc = run(hand_c, "eval_c", u64::from(n) * 16);
    let mut worst_d = 0.0f32;
    let mut worst_c = 0.0f32;
    for i in 0..n as usize {
        worst_d = worst_d.max((dist[i] - hd[i]).abs()).max((samp[4 * i] - hc[4 * i]).abs());
        worst_c = worst_c
            .max((samp[4 * i + 1] - hc[4 * i + 1]).abs())
            .max((samp[4 * i + 2] - hc[4 * i + 2]).abs());
    }
    eprintln!(
        "{n} points within 2 cm of the surface: distances within {worst_d:.2e} m, channels within {worst_c:.2e}"
    );
    assert!(worst_d <= 1e-6, "a distance {worst_d:.2e} m off");
    assert!(worst_c <= 1e-5, "a channel {worst_c:.2e} off");
}

/// The `.wrela` files of `dir` ([`files_under`] it), each with its text.
fn wrela_files(dir: &Path) -> Vec<(PathBuf, String)> {
    let with_text = |p: PathBuf| {
        let text = std::fs::read_to_string(&p).expect("read");
        (p, text)
    };
    files_under(dir, &["wrela"]).into_iter().map(with_text).collect()
}

/// A file's text without its comments (`//` to the line's end; wrela has no others).
fn code_of(text: &str) -> String {
    text.lines().map(|l| l.split("//").next().unwrap_or("")).collect::<Vec<_>>().join("\n")
}

/// AC7, layering: the engine doesn't depend on the herd, and no code in it names the herd's
/// modules or its grazer (its comments may cite them; M6's history has a herd of its own, a word
/// of the floor's map, not this example's); neither uses `unsafe`; and the compiler's crates name
/// nothing of the engine (the `wrela` command's tools, which find an `engine/` beside a parts
/// file, aren't the compiler). And AC9's "no ratio is met by hand": the engine and the herd are
/// wrela sources and their manifests alone, with no WGSL or JS of their own.
#[test]
fn the_engine_and_the_herd_keep_their_layers() {
    let root = repo_root();
    for dir in ["engine", "examples/herd"] {
        let listed = std::process::Command::new("git")
            .args(["ls-files", dir])
            .current_dir(&root)
            .output()
            .expect("git ls-files");
        for file in String::from_utf8_lossy(&listed.stdout).lines() {
            assert!(
                file.ends_with(".wrela") || file.ends_with("/wrela.toml"),
                "{file} isn't a wrela source or a manifest"
            );
        }
    }
    let manifest =
        std::fs::read_to_string(root.join("engine/wrela.toml")).expect("engine/wrela.toml");
    assert!(!manifest.contains("herd"), "the engine depends on the herd: {manifest}");
    for (path, text) in wrela_files(&root.join("engine")) {
        let code = code_of(&text);
        for word in ["grazer", "Grazer", "herd::", "unsafe"] {
            assert!(!code.contains(word), "{} has `{word}` in its code", path.display());
        }
    }
    for (path, text) in wrela_files(&root.join("examples/herd")) {
        assert!(!code_of(&text).contains("unsafe"), "{} uses `unsafe`", path.display());
    }
    for krate in ["syntax", "sema", "lower", "ir", "wasm", "wgsl", "driver", "diag"] {
        for p in files_under(&root.join("compiler").join(krate).join("src"), &["rs"]) {
            let text = std::fs::read_to_string(&p).expect("read");
            assert!(
                !text.contains("engine::") && !text.contains("\"engine\""),
                "{} names the engine",
                p.display()
            );
        }
    }
}
