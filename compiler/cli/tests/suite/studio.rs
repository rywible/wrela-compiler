//! Headless metadata and browser-only builds retain the lens's answers and artifacts.

use crate::common;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn package(name: &str) -> PathBuf {
    common::package(
        name,
        &[
            ("wrela.toml", &common::manifest("lens_subject")),
            ("main.wrela", common::FRAME),
            (
                "subject.wrela",
                "use std::field::{Surface, sphere}\n\npub fn subject() -> Surface {\n    sphere(0.375)\n}\n",
            ),
        ],
    )
}

fn answer(pkg: &Path, args: &[&str]) -> serde_json::Value {
    common::answer(common::wrela().arg("studio").arg(pkg).args(args))
}

#[test]
#[ignore = "needs the runtime"]
fn literals_answer_without_a_gpu_or_rendered_frame() {
    let pkg = package("studio-cpu-literals");
    let missing_driver = pkg.join("no-vulkan-driver.json");
    let out = common::wrela()
        .arg("studio")
        .arg(&pkg)
        .arg("literals")
        .env("VK_DRIVER_FILES", &missing_driver)
        .env("VK_ICD_FILENAMES", &missing_driver)
        .output()
        .expect("run wrela");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let got: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON answer");
    assert_eq!(
        got,
        serde_json::json!({
            "action": "literals",
            "count": 1,
            "literals": [{
                "index": 0, "package": "lens_subject", "file": "subject.wrela",
                "line": 4, "column": 12, "text": "0.375", "value": 0.375,
                "source": "    sphere(0.375)"
            }]
        })
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("wrela: "), "program logs echoed twice");
}

#[test]
#[ignore = "needs the runtime"]
fn synchronous_answers_need_no_gpu_or_rendered_frame() {
    let pkg = package("studio-cpu-queries");
    let missing_driver = pkg.join("no-vulkan-driver.json");
    let run = |args: &[&str]| {
        common::answer(
            common::wrela()
                .arg("studio")
                .arg(&pkg)
                .args(args)
                .env("VK_DRIVER_FILES", &missing_driver)
                .env("VK_ICD_FILENAMES", &missing_driver),
        )
    };
    let parts = run(&["parts"]);
    assert_eq!(parts["action"], "parts");
    assert_eq!(parts["count"], parts["parts"].as_array().unwrap().len());

    let probe = run(&["probe", "1", "0", "0"]);
    assert_eq!(probe["action"], "probe");
    assert_eq!(probe["distance"], 0.625);
    assert_eq!(probe["inside"], false);

    let ray = run(&["ray", "1", "0", "0", "-1", "0", "0"]);
    assert_eq!(ray["hit"], true);
    assert!((ray["distance"].as_f64().unwrap() - 0.625).abs() < 0.001, "{ray}");

    let measure = run(&["measure", "0", "0", "0", "3", "4", "0"]);
    assert_eq!(measure["distance"], 5.0);

    let spec = run(&["spec"]);
    assert_eq!(spec["action"], "spec");
    assert!(spec["error"].as_str().unwrap().contains("no spec"), "{spec}");

    let view = run(&["view", "0"]);
    assert_eq!(view["view"], "side");
    assert_eq!(view["framings"].as_array().unwrap().len(), 4);
}

fn artifacts(page: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("the build's artifacts") {
            let entry = entry.expect("an artifact entry");
            let path = entry.path();
            if path == root.join(".native") {
                continue;
            }
            if entry.file_type().expect("an artifact's type").is_dir() {
                walk(root, &path, files);
            } else {
                let name = path.strip_prefix(root).unwrap().to_str().unwrap().to_string();
                files.insert(name, std::fs::read(path).expect("an artifact"));
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(page, page, &mut files);
    files
}

#[test]
#[ignore = "needs the runtime"]
fn artifacts_only_matches_the_regular_build_without_native_compilation() {
    let pkg = package("studio-artifacts");
    common::stdout(
        common::wrela()
            .arg("studio")
            .arg(&pkg)
            .args(["build", "--artifacts-only"])
            .env_remove("WRELA_NATIVE_CACHE"),
    );
    let page = pkg.join("build/studio/page");
    let browser = artifacts(&page);
    assert!(browser.contains_key("game.wasm") && browser.contains_key("manifest.json"));
    assert!(browser.keys().any(|name| name.ends_with(".wgsl")), "no shader artifacts");
    assert!(!page.join(".native").exists(), "artifact checks must not compile native code");
    common::stdout(
        common::wrela().arg("studio").arg(&pkg).arg("build").env_remove("WRELA_NATIVE_CACHE"),
    );
    assert_eq!(artifacts(&page), browser, "native preparation changed the browser artifacts");
    assert!(
        std::fs::read_dir(page.join(".native"))
            .expect("the default build precompiles its WASM")
            .flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == "cwasm"))
    );
}

#[test]
fn artifacts_only_is_rejected_for_actions_before_building() {
    let pkg = package("studio-artifacts-action");
    for action in ["literals", "look", "serve"] {
        let out = common::wrela()
            .arg("studio")
            .arg(&pkg)
            .args([action, "--artifacts-only"])
            .output()
            .expect("run wrela");
        assert_eq!(out.status.code(), Some(2), "{action}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("only for `studio build`"));
    }
    assert!(!pkg.join("build").exists(), "an invalid flag must not start a build");
}

#[test]
fn invalid_actions_are_rejected_before_building() {
    let pkg = package("studio-invalid-action");
    for args in
        [vec!["no-such-action"], vec!["probe"], vec!["probe", "NaN", "0", "0"], vec!["view", "-1"]]
    {
        let out = common::wrela().arg("studio").arg(&pkg).args(&args).output().expect("run wrela");
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
    }
    assert!(!pkg.join("build").exists(), "an invalid action must not start a build");
}

#[test]
#[ignore = "needs the runtime"]
fn literals_with_a_png_keeps_the_same_answer_and_draws() {
    let pkg = package("studio-literals-png");
    let metadata = answer(&pkg, &["literals"]);
    let png = pkg.join("literals.png");
    let rendered = answer(&pkg, &["literals", "--png", png.to_str().unwrap(), "--size", "64x64"]);
    assert_eq!(rendered, metadata);
    let (w, h, pixels) = wrela_host::image::read_png(&png).expect("rendered PNG");
    assert_eq!((w, h), (64, 64));
    let first = &pixels[..4];
    assert!(pixels.chunks_exact(4).any(|p| p != first), "the lens drew no subject");

    let metadata = answer(&pkg, &["probe", "1", "0", "0"]);
    let rendered =
        answer(&pkg, &["probe", "1", "0", "0", "--png", png.to_str().unwrap(), "--size", "64x64"]);
    assert_eq!(rendered, metadata, "a CPU field query must match the rendered lens");
}
