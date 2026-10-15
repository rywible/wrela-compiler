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
