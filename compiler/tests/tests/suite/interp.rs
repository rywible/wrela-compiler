//! The interpreter of GPU code (`wrela_ir::interp`) against the GPU: texel loads and kernels that
//! read a texture interpret as the GPU ran them, on what the GPU held (`wrela_driver::captured`).

use crate::built;
use wrela_abi::stream::Command;
use wrela_host::{Host, Options};
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
