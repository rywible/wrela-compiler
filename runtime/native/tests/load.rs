//! Loading a build: what's rejected before the GPU is touched (no GPU needed).

mod common;

use wrela_host::{Error, Host};

fn load_with_manifest(name: &str, edit: impl Fn(&str) -> String) -> Error {
    let dir = common::first_light_with(
        name,
        &std::fs::read(common::first_light().join("game.wasm")).expect("wasm"),
    );
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).expect("manifest");
    std::fs::write(dir.join("manifest.json"), edit(&manifest)).expect("write");
    let err = Host::load(&dir).err().expect("rejected");
    std::fs::remove_dir_all(&dir).expect("cleanup");
    err
}

#[test]
fn rejects_another_manifest_version() {
    let err = load_with_manifest("manifest-v2", |m| {
        m.replace("\"manifest_version\": 1", "\"manifest_version\": 2")
    });
    assert!(matches!(&err, Error::Manifest(_)), "{err}");
    assert!(err.to_string().contains("manifest version 2, but this host reads version 1"), "{err}");
}

#[test]
fn rejects_another_stream_version() {
    let err = load_with_manifest("stream-v9", |m| {
        m.replace("\"stream_version\": 2", "\"stream_version\": 9")
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
        r#"(module (import "env" "now" (func (result f64))) (memory (export "memory") 1)
             (func (export "frame") (param f32 i32 i32)))"#,
    )
    .expect("compiles");
    let dir = common::first_light_with("other-import", &wasm);
    let err = Host::load(&dir).err().expect("rejected");
    assert_eq!(
        err.to_string(),
        "invalid program: it imports `env.now`, but a wrela program may import only `wrela.submit`"
    );
    std::fs::remove_dir_all(&dir).expect("cleanup");
}
