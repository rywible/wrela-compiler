//! Loading a build: what's rejected before the GPU is touched (no GPU needed).

use crate::common;

use wrela_host::{CpuBuild, Error, Host};

fn load_with_manifest(name: &str, edit: impl Fn(&str) -> String) -> Error {
    let dir = common::first_light_copy(name);
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).expect("manifest");
    std::fs::write(dir.join("manifest.json"), edit(&manifest)).expect("write");
    Host::load(&dir).err().expect("rejected")
}

#[test]
fn rejects_another_manifest_version() {
    let err = load_with_manifest("manifest-v1", |m| {
        m.replace(
            &format!("\"manifest_version\": {}", wrela_abi::manifest::VERSION),
            "\"manifest_version\": 1",
        )
    });
    assert!(matches!(&err, Error::Manifest(_)), "{err}");
    let want =
        format!("manifest version 1, but this host reads version {}", wrela_abi::manifest::VERSION);
    assert!(err.to_string().contains(&want), "{err}");
}

#[test]
fn rejects_another_stream_version() {
    let other = wrela_abi::stream::VERSION + 1;
    let err = load_with_manifest("stream-other", |m| {
        m.replace(
            &format!("\"stream_version\": {}", wrela_abi::stream::VERSION),
            &format!("\"stream_version\": {other}"),
        )
    });
    assert!(matches!(&err, Error::Manifest(_)), "{err}");
    assert!(err.to_string().contains(&format!("command stream version {other}")), "{err}");
}

#[test]
fn reports_missing_files() {
    let err = load_with_manifest("missing-shader", |m| m.replace("fill.wgsl", "nope.wgsl"));
    assert!(matches!(&err, Error::Io { path, .. } if path.ends_with("nope.wgsl")), "{err}");
    let err = Host::load(common::temp_dir("empty")).err().expect("rejected");
    assert!(matches!(&err, Error::Io { path, .. } if path.ends_with("manifest.json")), "{err}");
}

#[test]
fn cpu_frame_builds_need_no_emitted_shaders() {
    let dir = common::first_light_copy("cpu-without-shaders");
    std::fs::remove_file(dir.join("fill.wgsl")).expect("remove compute shader");
    std::fs::remove_file(dir.join("first-light.wgsl")).expect("remove render shader");
    assert!(matches!(CpuBuild::load_metered(&dir, 1_000_000), Err(Error::Io { .. })));
    let mut host = CpuBuild::load_metered_cpu(&dir, 1_000_000)
        .expect("CPU build loads")
        .start_with(1)
        .expect("init's dispatch is checked without running a shader");
    host.frame(0.0, 64, 48).expect("the frame's draw is checked without running a shader");
    assert_eq!(
        host.call_export("tri", &[wrela_host::Value::F32(0.25)]).unwrap(),
        [wrela_host::Value::F32(0.0)]
    );
}

#[test]
fn rejects_a_program_with_other_imports() {
    let wasm = wat::parse_str(
        r#"(module (import "env" "now" (func (result f64))) (import "wrela" "memory" (memory 1 16384 shared)) (export "memory" (memory 0))
             (func (export "frame") (param f32 i32 i32)))"#,
    )
    .expect("compiles");
    let dir = common::first_light_with("other-import", &wasm);
    let err = Host::load(&dir).err().expect("rejected");
    assert_eq!(
        err.to_string(),
        "invalid program: it imports `env.now`, which isn't one of the host's: `wrela.memory`, `wrela.submit`, `wrela.request_status`, `wrela.request_take`, `wrela.limit`, `wrela.audio`, `wrela.input`, `wrela.payload`, `wrela.tick`, `wrela.keep`, `wrela.kept`, `wrela.clock`, `wrela.print`, `wrela.phase`"
    );
}

#[test]
fn a_build_cannot_supply_executable_code_from_its_native_cache() {
    let wasm = wat::parse_str(
        r#"(module
            (import "wrela" "memory" (memory 1 16384 shared))
            (export "memory" (memory 0))
            (func (export "frame") (param f32 i32 i32))
            (func (export "cache_marker") (result i32) i32.const 17))"#,
    )
    .expect("program WASM");
    let dir = common::build("poisoned-native-cache", &common::manifest("[]"), &[], &wasm);

    // A build could place an artifact with the old cache's exact name. The artifact is valid
    // Wasmtime code for this engine but contains a different module than the build's WASM.
    let mut config = wasmtime::Config::new();
    config.wasm_threads(true).shared_memory(true).consume_fuel(false);
    config.epoch_interruption(false).native_unwind_info(false);
    let engine = wasmtime::Engine::new(&config).expect("engine");
    let wrong = wat::parse_str("(module)").expect("other WASM");
    let poison = wasmtime::Module::new(&engine, &wrong)
        .expect("other module")
        .serialize()
        .expect("artifact");
    let old_cache = dir.join(".native");
    std::fs::create_dir_all(&old_cache).expect("legacy cache directory");
    let name = format!(
        "game-{}-{}.cwasm",
        wrela_abi::hash::hex(wrela_abi::ticks::wasm_hash(&wasm)),
        std::env::consts::ARCH
    );
    let planted = old_cache.join(name);
    std::fs::write(&planted, &poison).expect("planted artifact");

    let mut host = CpuBuild::load_cpu(&dir)
        .expect("build WASM, not its planted native code")
        .start_with(1)
        .expect("start");
    assert_eq!(
        host.call_export("cache_marker", &[]).expect("the build's export"),
        [wrela_host::Value::I32(17)]
    );
    assert_eq!(std::fs::read(&planted).expect("planted artifact remains"), poison);
    let _ = std::fs::remove_file(&planted);
}

/// A driven build (the tool host's `drive`, language.md §15.5), read through its directory's
/// handle, its code given for the engine `wrela-host` runs on, runs as a loaded one: the same
/// frames give the same hash.
#[test]
fn a_driven_build_runs_as_a_loaded_one() {
    let dir = common::first_light_copy("driven");
    let handle = wrela_host::open_build_dir(&dir).expect("the build's directory");
    let driven = CpuBuild::load_driven(handle, &dir, &wrela_host::compiling()).expect("driven");
    let loaded = CpuBuild::load(&dir).expect("loaded");
    let mut hashes = Vec::new();
    for build in [driven, loaded] {
        let mut host = build.start_with(1).expect("start");
        for i in 0..3 {
            host.lockstep_frame(i, 60.0, 64, 48, &[]).expect("a frame");
        }
        hashes.push(host.hash());
    }
    assert_eq!(hashes[0], hashes[1]);
}
