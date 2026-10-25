//! The GPU's real limits (AC10): both hosts open the device with the adapter's limits, a program
//! sees them through `std::gpu::limits()`, and commands are checked against them, so a texture
//! may be as wide as the GPU allows, past WebGPU's default 8192. Needs a GPU:
//! `cargo test -p wrela-tests --test suite gpu_limits:: -- --ignored`.

use wrela_host::Host;
use wrela_tests::{one_u32, page};

#[test]
#[ignore = "needs a GPU"]
fn a_program_sees_the_adapters_limits() {
    let (dir, _) = page("compiler/tests/gpu-limits", "gpu-limits-native");
    let mut host = Host::load(&dir).expect("load");
    // What wgpu says of the adapter, through the same device request.
    let (device, _) =
        wrela_host::open_device("limits test", wgpu::Features::empty()).expect("device");
    let l = device.limits();
    let texture = one_u32(&mut host, "max_texture_size", &[]);
    assert_eq!(texture, l.max_texture_dimension_2d);
    let buffer = l.max_buffer_size.min(l.max_storage_buffer_binding_size).min(u64::from(u32::MAX));
    assert_eq!(u64::from(one_u32(&mut host, "max_buffer_size", &[])), buffer);
    assert_eq!(
        one_u32(&mut host, "max_workgroups_per_dimension", &[]),
        l.max_compute_workgroups_per_dimension
    );
    // At least WebGPU's defaults; the texture `init` made is as wide as the limit.
    assert!(texture >= 8192, "a texture limit of {texture}");
    assert_eq!(one_u32(&mut host, "wide", &[]), texture);
    eprintln!("the adapter's textures go to {texture} texels, its buffers to {buffer} bytes");
}

/// `std::gpu::memory()` is std's count, the host's `memory.json` its own: for a program that
/// makes known resources and drops a buffer (compiler/tests/memory), Chrome's count at the end
/// and at its most is the program's, buffers and textures together.
#[test]
#[ignore = "needs the runtime"]
fn std_counts_the_gpu_memory_the_host_does() {
    let (_, rel) = wrela_tests::page("compiler/tests/memory", "memory-browser");
    let run = wrela_tests::run_in_chrome(&rel, 4, 64, 64, 60.0);
    let last = run.printed.last().expect("a line each frame").1.clone();
    let n: Vec<u64> =
        last.split_whitespace().skip(1).map(|w| w.parse().expect("a count")).collect();
    let [buffers, textures, peak_buffers, peak_textures] = n[..] else { panic!("{last}") };
    let results = wrela_tests::repo_root().join(&rel).join("results");
    let memory = wrela_tests::result_json(&results, "memory.json");
    assert_eq!(memory["gpu"]["bytes"].as_u64(), Some(buffers + textures), "{memory}");
    // The program's two most were held at once, before the buffer dropped.
    assert_eq!(memory["gpu"]["peak"].as_u64(), Some(peak_buffers + peak_textures), "{memory}");
}
