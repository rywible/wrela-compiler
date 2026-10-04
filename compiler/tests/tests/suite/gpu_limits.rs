//! The GPU's real limits (AC10): both hosts open the device with the adapter's limits, a program
//! sees them through `std::gpu::limits()`, and commands are checked against them, so a texture
//! may be as wide as the GPU allows, past WebGPU's default 8192. Needs a GPU:
//! `cargo test -p wrela-tests --test suite gpu_limits:: -- --ignored`.

use wrela_host::{Host, Value};
use wrela_tests::page;

fn u32_export(host: &mut Host, name: &str, args: &[Value]) -> u32 {
    match host.call_export(name, args).expect(name).as_slice() {
        [Value::I32(v)] => *v as u32,
        other => panic!("{name} returned {other:?}"),
    }
}

#[test]
#[ignore = "needs a GPU"]
fn a_program_sees_the_adapters_limits() {
    let (dir, _) = page("compiler/tests/gpu-limits", "gpu-limits-native");
    let mut host = Host::load(&dir).expect("load");
    // What wgpu says of the adapter, through the same device request.
    let (device, _) =
        wrela_host::open_device("limits test", wgpu::Features::empty()).expect("device");
    let l = device.limits();
    let texture = u32_export(&mut host, "max_texture_size", &[]);
    assert_eq!(texture, l.max_texture_dimension_2d);
    let buffer = l.max_buffer_size.min(l.max_storage_buffer_binding_size).min(u64::from(u32::MAX));
    assert_eq!(u64::from(u32_export(&mut host, "max_buffer_size", &[])), buffer);
    assert_eq!(
        u32_export(&mut host, "max_workgroups_per_dimension", &[]),
        l.max_compute_workgroups_per_dimension
    );
    // At least WebGPU's defaults; the texture `init` made is as wide as the limit.
    assert!(texture >= 8192, "a texture limit of {texture}");
    assert_eq!(u32_export(&mut host, "wide", &[]), texture);
    eprintln!("the adapter's textures go to {texture} texels, its buffers to {buffer} bytes");
}
