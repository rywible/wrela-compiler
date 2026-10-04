//! Loading a build: what's rejected before the GPU is touched (no GPU needed).

use crate::common;

use wrela_host::{Error, Host};

fn load_with_manifest(name: &str, edit: impl Fn(&str) -> String) -> Error {
    let dir = common::first_light_copy(name);
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).expect("manifest");
    std::fs::write(dir.join("manifest.json"), edit(&manifest)).expect("write");
    Host::load(&dir).err().expect("rejected")
}

#[test]
fn rejects_another_manifest_version() {
    let err = load_with_manifest("manifest-v1", |m| {
        m.replace("\"manifest_version\": 2", "\"manifest_version\": 1")
    });
    assert!(matches!(&err, Error::Manifest(_)), "{err}");
    assert!(err.to_string().contains("manifest version 1, but this host reads version 2"), "{err}");
}

#[test]
fn rejects_another_stream_version() {
    let err = load_with_manifest("stream-v9", |m| {
        m.replace("\"stream_version\": 4", "\"stream_version\": 9")
    });
    assert!(matches!(&err, Error::Manifest(_)), "{err}");
    assert!(err.to_string().contains("command stream version 9"), "{err}");
}

#[test]
fn reports_missing_files() {
    let err = load_with_manifest("missing-shader", |m| m.replace("fill.wgsl", "nope.wgsl"));
    assert!(matches!(&err, Error::Io { path, .. } if path.ends_with("nope.wgsl")), "{err}");
    let err = Host::load(common::temp_dir("empty")).err().expect("rejected");
    assert!(matches!(&err, Error::Io { path, .. } if path.ends_with("manifest.json")), "{err}");
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
        "invalid program: it imports `env.now`, which isn't one of the host's: `wrela.memory`, `wrela.submit`, `wrela.request_status`, `wrela.request_take`, `wrela.limit`, `wrela.audio`"
    );
}
