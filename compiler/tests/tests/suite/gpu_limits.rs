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

/// A level as wide and tall as the adapter allows can hold more bytes than a `u32` counts (16384
/// × 16384 `Rgba32Float` texels are 4 GiB): std counts a write's, a copy's and a read's bytes in
/// `u64`, and one that can't hold them panics, saying why, rather than overflow.
#[test]
#[ignore = "needs a GPU"]
fn the_largest_levels_bytes_are_counted_past_4_gib() {
    let text = "use std::gpu::{
    GpuBuffer, Rgba32Float, Texture, buffer, copy_buffer_to_texture, copy_texture_to_buffer,
    limits, read_texture, texture,
}

pub fn frame(time: f32, width: u32, height: u32) {}

pub fn side() -> u32 {
    limits().max_texture_size
}

fn largest() -> Texture<Rgba32Float> {
    texture(width: 16384, height: 16384)
}

pub fn write() {
    var t = largest()
    t.write(x: 0, y: 0, width: 16384, height: 16384, bytes: [u8(0); 16])
}

pub fn to_buffer() {
    var b: GpuBuffer<u32> = buffer(64)
    copy_texture_to_buffer(largest().level(0), mut b)
}

pub fn from_buffer() {
    let b: GpuBuffer<u32> = buffer(64)
    var t = largest()
    copy_buffer_to_texture(b.span(start: 0, count: 64), t.level_mut(0))
}

pub fn read() {
    let _ = read_texture(largest().level(0))
}
";
    let dir = crate::package("gpu-limits-largest", text);
    wrela_tests::must_build(&dir, &dir.join("build"));
    let mut host = Host::load(dir.join("build")).expect("load");
    let side = one_u32(&mut host, "side", &[]);
    assert!(side >= 16384, "the adapter's textures go to {side} texels; this needs 16384");
    for (export, why) in [
        ("write", "write: the bytes aren't the rectangle's texels"),
        ("to_buffer", "copy_texture_to_buffer: the buffer is smaller than the texture's"),
        ("from_buffer", "copy_buffer_to_texture: the span is smaller than the texture's"),
        ("read", "read_texture: the level's padded rows are 4 GiB or more"),
    ] {
        let err = host.call_export(export, &[]).expect_err(export);
        assert!(format!("{err:#}").contains(why), "{export}: {err:#}");
    }
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

/// Pipeline tiers (compiler/tests/tiers, language.md §18.9): the manifest marks `mark`, which
/// only frame 1 dispatches, tier 1 (`conditional`) and `fill` tier 0; Chrome reports tier 0's
/// time as `ms` and every pipeline's as `tier1_ms` (`pipelines.json`), and the frames run.
#[test]
#[ignore = "needs the runtime"]
fn tier_1_builds_after_tier_0_and_is_reported() {
    let (dir, rel) = wrela_tests::page("compiler/tests/tiers", "tiers-browser");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("manifest.json")).expect("manifest"),
    )
    .expect("JSON");
    let tiers: Vec<(&str, bool)> = manifest["pipelines"]
        .as_array()
        .expect("pipelines")
        .iter()
        .map(|p| (p["name"].as_str().unwrap_or(""), p["conditional"].as_bool().unwrap_or(false)))
        .collect();
    assert_eq!(tiers, [("fill", false), ("mark", true)]);
    wrela_tests::run_in_chrome_with(
        &rel,
        wrela_tests::ChromeRun { salt: 1, ..wrela_tests::ChromeRun::new(4, 64, 64, 60.0) },
    );
    let p = wrela_tests::result_json(&dir.join("results"), "pipelines.json");
    eprintln!("pipelines.json: {p}");
    assert_eq!(p["tier1"].as_u64(), Some(1), "{p}");
    let (tier0, all) = (p["ms"].as_f64().expect("ms"), p["tier1_ms"].as_f64().expect("tier1_ms"));
    assert!(tier0 > 0.0 && all >= tier0, "{p}");
}
