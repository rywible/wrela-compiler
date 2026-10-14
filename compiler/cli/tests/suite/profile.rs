//! `wrela profile`: a program with a labelled dispatch every frame, a labelled one in its first
//! frame alone, and phases its frames time; the report says each, and its WGSL.

use crate::common;

const WORK: &str = "use std::gpu::{GlobalId, GpuBuffer, Slots, buffer, dispatch, label}
use std::time::phase

@compute(64)
fn fill(out: mut Slots<f32>, id: GlobalId) {
    out[id] = f32(id.x)
}

pub struct Work {
    points: GpuBuffer<f32>,
    frames: u32,
}

pub fn init() -> Work {
    Work { points: buffer(4096), frames: 0 }
}

pub fn frame(w: mut Work, time: f32, width: u32, height: u32) {
    assert(abs(time - f32(w.frames) / 60.0) < 0.000001, \"frame clock did not reset\")
    phase(\"record\")
    if w.frames == 0 {
        label(\"setup\")
        dispatch(fill.bind(out: mut w.points), groups: 64)
    }
    label(\"each frame\")
    dispatch(fill.bind(out: mut w.points), groups: 64)
    w.frames += 1
    phase(\"\")
}
";

#[test]
fn profile_reports_the_gpu_by_label_the_cpu_and_the_wgsl() {
    let dir = common::package(
        "profile-work",
        &[("wrela.toml", &common::manifest("work")), ("main.wrela", WORK)],
    );
    let d = dir.to_str().unwrap();
    let out = common::wrela()
        .args(["profile", d, "--frames", "4", "--warmup", "0", "--json"])
        .output()
        .expect("run wrela");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{text}{}", String::from_utf8_lossy(&out.stderr));
    let p: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(p["frames"], 4);
    let label = |name: &str| {
        p["gpu"]["labels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["label"] == name)
            .unwrap_or_else(|| panic!("no `{name}` in {text}"))
            .clone()
    };
    assert_eq!(label("each frame")["frames"], 4);
    assert_eq!(label("setup")["frames"], 1);
    assert!(label("each frame")["median_ms"].as_f64().unwrap() >= 0.0);
    let phases = p["cpu"]["phases"].as_array().unwrap();
    assert_eq!(phases.len(), 1, "{text}");
    assert_eq!(phases[0]["name"], "record");
    assert_eq!(phases[0]["times"], 4);
    let wgsl = p["wgsl"].as_array().unwrap();
    assert_eq!(wgsl.len(), 1, "{text}");
    assert_eq!(wgsl[0]["pipeline"], "fill");
    assert!(wgsl[0]["bytes"].as_u64().unwrap() > 0);
    // The slow frames beside the median: a 95th percentile is never under it.
    let each = label("each frame");
    assert!(each["p95_ms"].as_f64().unwrap() >= each["median_ms"].as_f64().unwrap(), "{text}");
    assert!(p["cpu"]["frame_p95_ms"].as_f64().unwrap() >= p["cpu"]["frame_ms"].as_f64().unwrap());

    // Compared with a baseline, each time says what it was, and a label the baseline ran that
    // this run didn't is listed.
    let mut before = p.clone();
    before["gpu"]["labels"].as_array_mut().unwrap().push(serde_json::json!({
        "label": "removed since", "median_ms": 1.5, "p95_ms": 2.0, "total_ms": 6.0, "frames": 4,
    }));
    let baseline = dir.join("before.json");
    std::fs::write(&baseline, before.to_string()).unwrap();
    let b = baseline.to_str().unwrap();
    let out = common::wrela()
        .args(["profile", d, "--frames", "4", "--warmup", "0", "--json", "--baseline", b])
        .output()
        .expect("run wrela");
    let compared: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    let each = compared["gpu"]["labels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["label"] == "each frame")
        .unwrap()
        .clone();
    assert_eq!(each["was_median_ms"], label("each frame")["median_ms"], "{compared}");
    assert_eq!(compared["cpu"]["was_frame_ms"], p["cpu"]["frame_ms"]);
    assert_eq!(compared["baseline"]["gone"], serde_json::json!(["removed since"]));
    let report = common::stdout(
        common::wrela()
            .args(["profile", d, "--frames", "4", "--warmup", "0"])
            .args(["--baseline", b]),
    );
    assert!(report.contains("was ") && report.contains("no longer run: removed since"), "{report}");

    for field in ["environment", "workload", "schema"] {
        let mut incompatible = before.clone();
        incompatible[field] = serde_json::json!({"different": true});
        std::fs::write(&baseline, incompatible.to_string()).unwrap();
        let out = common::wrela()
            .args(["profile", d, "--frames", "4", "--warmup", "0", "--baseline", b])
            .output()
            .unwrap();
        assert!(!out.status.success(), "accepted different {field}");
    }

    let saved = dir.join("saved-build");
    common::stdout(common::wrela().args(["build", d, "-o", saved.to_str().unwrap()]));
    std::fs::write(dir.join("main.wrela"), WORK.replace("each frame", "candidate frame")).unwrap();
    let out = common::wrela()
        .args([
            "profile",
            d,
            "--frames",
            "4",
            "--warmup",
            "2",
            "--workers",
            "1",
            "--compare",
            saved.to_str().unwrap(),
            "--pairs",
            "3",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let comparison: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        comparison["order"],
        serde_json::json!([
            "baseline",
            "candidate",
            "candidate",
            "baseline",
            "baseline",
            "candidate"
        ])
    );
    assert_ne!(comparison["baseline_build_sha256"], comparison["candidate_build_sha256"]);
    for side in comparison["samples"].as_object().unwrap().values() {
        assert_eq!(side.as_array().unwrap().len(), 3);
        for sample in side.as_array().unwrap() {
            // Every restarted program discards setup work and phases from warmup.
            let labels = sample["gpu"]["labels"].as_array().unwrap();
            assert_eq!(labels.len(), 1, "warmup leaked: {sample}");
            assert_eq!(labels[0]["frames"], 4);
            assert_eq!(sample["cpu"]["phases"][0]["times"], 4);
            assert_eq!(sample["warmup"], 2);
            assert_eq!(sample["workload"]["workers"], 1);
        }
    }
    let labels = comparison["labels"].as_array().unwrap();
    assert_eq!(
        labels.iter().find(|l| l["label"] == "each frame").unwrap()["comparison"]["status"],
        "removed"
    );
    assert_eq!(
        labels.iter().find(|l| l["label"] == "candidate frame").unwrap()["comparison"]["status"],
        "added"
    );

    let generated = dir.join("build/profile");
    let manifest = std::fs::read(generated.join("manifest.json")).unwrap();
    let out = common::wrela()
        .args(["profile", d, "--compare", generated.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("would be overwritten"));
    assert_eq!(std::fs::read(generated.join("manifest.json")).unwrap(), manifest);
}
