//! The interpreter of GPU code (`wrela_ir::interp`) against the GPU, and `wrela pixel`'s probe
//! (`wrela_host::Probe`) and geometry (`wrela_driver::pixel`): texel loads and kernels that read
//! a texture interpret as the GPU ran them; a probe shows each draw into a pixel and draws the
//! same picture; the fragment found for a pixel has the GPU's varyings; `fwidth` through the
//! block of four is the GPU's; and a division by zero names its line.

use crate::built;
use wrela_abi::stream::Command;
use wrela_driver::pixel::{Query, Report, What};
use wrela_host::{Host, Options, Probe};
use wrela_ir::interp::Interp;
use wrela_ir::interp::concrete::{floats_of, uvec3};

fn modules(pkg: &str) -> Vec<wrela_driver::GpuModule> {
    let dir = wrela_tests::repo_root().join("compiler/tests").join(pkg);
    let kind = wrela_driver::BuildKind { debug: false, testing: true };
    let out = wrela_driver::gpu_modules(&dir, kind).unwrap_or_else(|out| {
        panic!("{}", wrela_diag::render::render_all(&out.sources, &out.diagnostics))
    });
    out.gpu
}

/// A texture's loads, interpreted, are the GPU's, exactly: `texels`' `show` at 64 pixels, its
/// texture (written by a kernel) read back from the GPU.
#[test]
#[ignore = "needs a GPU"]
fn texel_loads_interpret_as_the_gpu_read_them() {
    let gpu = modules("texels");
    let mut host =
        Host::load_with(built("texels"), &Options { record: true, ..Options::default() })
            .expect("load");
    host.frame(0.0, 128, 128).expect("a frame");
    let batches = host.take_batches();
    let screen = host.read_screen().expect("the screen");
    let (pipeline, bindings, uniforms) = batches
        .iter()
        .flat_map(|b| wrela_abi::stream::decode(b).expect("decodes"))
        .find_map(|c| match c {
            Command::Draw { pipeline, bindings, uniforms, .. } => {
                Some((pipeline, bindings, uniforms.to_vec()))
            }
            _ => None,
        })
        .expect("a draw");
    let g = &gpu[pipeline as usize];
    let samplers = wrela_driver::captured::samplers(&batches);
    let given = wrela_driver::captured::given(
        g.manifest.as_ref().expect("a manifest entry"),
        &bindings,
        &uniforms,
        &samplers,
        &mut host,
    )
    .expect("captured");
    let fe = g.entries.iter().position(|e| e == "show").expect("`show`");
    let mut differ = 0;
    for k in 0..64u32 {
        let (x, y) = (k * 2 + 1, (k * 37) % 128);
        let mut gv = given.clone();
        let pos = wrela_ir::interp::concrete::fvec(&[x as f32 + 0.5, y as f32 + 0.5, 0.5, 1.0]);
        gv.builtins = vec![(wrela_ir::BuiltinInput::Position, pos)];
        let out = Interp::new(&g.module, &mut gv, &mut ()).run_entry(fe).expect("runs");
        let c = floats_of(out.result.as_ref().expect("a colour"));
        let at = ((y * 128 + x) * 4) as usize;
        for i in 0..4 {
            let want = screen[at + i];
            let got = (c[i].clamp(0.0, 1.0) * 255.0).round() as u8;
            if got != want {
                differ += 1;
            }
        }
    }
    assert_eq!(differ, 0, "channels that differ");
}

/// The kernels of `sampled` that read a cooked 3D texture by loads (`Sampled3d`), interpreted
/// on what they were given, write the GPU's values within 1/255 (the GPU's shader compiler
/// may reassociate; nothing here is a sample through a sampler).
#[test]
#[ignore = "needs a GPU"]
fn kernels_that_read_a_texture_interpret_as_the_gpu_ran_them() {
    let gpu = modules("sampled");
    let mut host =
        Host::load_with(built("sampled"), &Options { record: true, ..Options::default() })
            .expect("load");
    host.frame(0.0, 64, 64).expect("a frame");
    let batches = host.take_batches();
    let samplers = wrela_driver::captured::samplers(&batches);
    let mut checked = 0;
    let mut worst = 0.0f64;
    for cmd in batches.iter().flat_map(|b| wrela_abi::stream::decode(b).expect("decodes")) {
        let Command::Dispatch { pipeline, groups, bindings, uniforms } = cmd else { continue };
        let g = &gpu[pipeline as usize];
        let manifest = g.manifest.as_ref().expect("a manifest entry");
        let reads_texture =
            manifest.bindings.iter().any(|b| b.kind == wrela_abi::manifest::BindingKind::Texture3d);
        if !reads_texture {
            continue;
        }
        let given =
            wrela_driver::captured::given(manifest, &bindings, uniforms, &samplers, &mut host)
                .expect("captured");
        let out = manifest
            .bindings
            .iter()
            .zip(&bindings)
            .find(|(b, _)| b.kind == wrela_abi::manifest::BindingKind::ReadWrite)
            .map(|(_, b)| wrela_tests::f32s(&host.read_buffer(b.handle).expect("read")))
            .expect("an output");
        for id in 0..64u32 {
            let mut gv = given.clone();
            gv.bytes.retain(|_, _| true);
            gv.builtins = vec![
                (wrela_ir::BuiltinInput::GlobalInvocationId, uvec3(id, 0, 0)),
                (wrela_ir::BuiltinInput::NumWorkgroups, uvec3(groups[0], groups[1], groups[2])),
            ];
            let r = Interp::new(&g.module, &mut gv, &mut ()).run_entry(0).expect("runs");
            let w = r.writes.iter().find(|w| w.index == id).expect("a write");
            let v = floats_of(&w.value)[0];
            let d = (f64::from(v) - f64::from(out[id as usize])).abs();
            worst = wrela_tests::worse(worst, d);
            checked += 1;
        }
    }
    assert!(checked >= 3 * 64, "only {checked} invocations checked");
    println!("{checked} invocations of `sampled`'s texture kernels: worst difference {worst:e}");
    assert!(worst <= 1.0 / 255.0, "worst difference {worst:e}");
}

/// A query of the pixel fixture's frame 0 at `at`, 64 × 64.
fn query(at: [u32; 2], draw: Option<usize>) -> Report {
    let gpu = modules("pixel");
    let q = Query {
        frame: 0,
        size: (64, 64),
        script: Vec::new(),
        fps: 60.0,
        what: What::Pixel { at, draw, fragment: None },
    };
    wrela_driver::pixel::query(&built("pixel"), &gpu, &q).expect("the query")
}

/// A pixel under both triangles: the history shows two draws, the large triangle's from the
/// clear colour to its colour, then the small one's to orange.
#[test]
#[ignore = "needs a GPU"]
fn a_probe_shows_each_draw_into_a_pixel() {
    let r = query([27, 36], None);
    let changed: Vec<_> = r.history.iter().filter(|h| h.after != h.before).collect();
    assert_eq!(changed.len(), 2, "{:?}", r.history);
    assert_eq!((changed[0].draw, changed[1].draw), (1, 2));
    assert_eq!(changed[0].before, [0, 0, 0, 255]);
    // 0.9 is 229.5 of 255: the GPU writes 229.
    assert_eq!(changed[1].after, [229, 128, 26, 255]);
    assert_eq!(changed[1].before, changed[0].after);
    assert_eq!(r.traced, Some(2));
}

/// A probe that lets every draw run draws the same picture as no probe.
#[test]
#[ignore = "needs a GPU"]
fn a_probe_draws_the_same_picture() {
    let picture = |probe: Option<Probe>| {
        let mut host = Host::load(built("pixel")).expect("load");
        host.set_probe(probe);
        host.frame(0.0, 64, 64).expect("a frame");
        host.read_screen().expect("the screen")
    };
    assert_eq!(picture(None), picture(Some(Probe { frame: 0, draws: 4 })));
    assert_ne!(picture(None), picture(Some(Probe { frame: 0, draws: 1 })));
}

/// The fragment found for a pixel inside the large triangle alone, its varyings interpolated
/// perspective-correct, gives the GPU's colour within 1/255.
#[test]
#[ignore = "needs a GPU"]
fn the_fragment_found_has_the_gpus_varyings() {
    for at in [[12, 51], [20, 40], [8, 20], [40, 55]] {
        let r = query(at, None);
        assert_eq!(r.traced, Some(1), "{at:?}: {:?}", r.history);
        let (got, want) = (r.interpreted.expect("interpreted"), r.gpu.expect("the GPU's"));
        for i in 0..4 {
            assert!((got[i] - want[i]).abs() <= 1.0 / 255.0 + 1e-6, "{at:?}: {got:?} vs {want:?}");
        }
    }
}

/// `fwidth` of a varying, through the block of four, is the GPU's within 2/255.
#[test]
#[ignore = "needs a GPU"]
fn fwidth_through_the_block_of_four_is_the_gpus() {
    for at in [[20, 0], [21, 1], [50, 0]] {
        let r = query(at, None);
        assert_eq!(r.traced, Some(3), "{at:?}: {:?}", r.history);
        assert!(r.passes >= 2, "{at:?}: {} passes", r.passes);
        let (got, want) = (r.interpreted.expect("interpreted"), r.gpu.expect("the GPU's"));
        for i in 0..4 {
            assert!((got[i] - want[i]).abs() <= 2.0 / 255.0 + 1e-6, "{at:?}: {got:?} vs {want:?}");
        }
    }
}

/// The interpreter against the GPU on a real shader: `fields`' `shade` (a sphere trace of the
/// grazer, lit by its derived gradient), interpreted at 100 pixels of a 128 × 128 frame, is
/// within 2/255 of the GPU's in every channel.
#[test]
#[ignore = "needs a GPU"]
fn the_fields_shade_interprets_as_the_gpu_drew_it() {
    let gpu = modules("fields");
    let q = Query {
        frame: 0,
        size: (128, 128),
        script: Vec::new(),
        fps: 60.0,
        what: What::Pixel { at: [0, 0], draw: None, fragment: None },
    };
    let pixels: Vec<[u32; 2]> = (0..100).map(|i| [6 + (i % 10) * 12, 6 + (i / 10) * 12]).collect();
    let traced =
        wrela_driver::pixel::query_pixels(&built("fields"), &gpu, &q, 1, &pixels).expect("traced");
    let mut worst = 0.0f64;
    let mut at_worst = [0, 0];
    for (p, (interpreted, after)) in pixels.iter().zip(&traced) {
        let got = interpreted.as_ref().expect("a fragment at every pixel");
        for i in 0..4 {
            let d = (f64::from(got[i].clamp(0.0, 1.0)) - f64::from(after[i]) / 255.0).abs();
            if d.is_nan() || d > worst {
                worst = d;
                at_worst = *p;
            }
        }
    }
    println!(
        "fields' shade at 100 pixels: worst difference {worst:.5} ({:.2}/255) at {at_worst:?}",
        worst * 255.0
    );
    assert!(worst <= 2.0 / 255.0, "worst {worst} at {at_worst:?}");
}
