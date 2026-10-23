//! Same-machine comparisons of frozen builds. Results are evidence, never correctness gates.

use super::*;
use serde_json::{Value, json as value};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};

pub(super) struct Storage(pub PathBuf);

impl Storage {
    pub(super) fn new() -> Result<Self, String> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let path = std::env::temp_dir()
            .join(format!("wrela-profile-storage-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&path).map_err(|e| e.to_string())?;
        Ok(Self(path))
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn validate_saved_build(package: &Path, before: &Path) -> Result<(), String> {
    let before = before.canonicalize().map_err(|e| format!("can't read baseline build: {e}"))?;
    let generated = package.canonicalize().map_err(|e| e.to_string())?.join("build/profile");
    let generated = generated.canonicalize().unwrap_or(generated);
    if before.starts_with(&generated) || generated.starts_with(&before) {
        return Err("baseline build would be overwritten; save it outside build/profile with `wrela build -o <dir>`".into());
    }
    build_hash(&before)?;
    Ok(())
}

pub(super) fn lease(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("can't open measurement lock {}: {e}", path.display()))?;
    file.try_lock().map_err(|e| {
        format!("measurement lock unavailable: {e}; another profiler may be running")
    })?;
    Ok(file)
}

pub(super) fn environment(host: &mut Host) -> Result<Value, String> {
    let machine = std::fs::read("/etc/machine-id")
        .or_else(|_| {
            let out = std::process::Command::new("hostname").output()?;
            if out.status.success() {
                Ok(out.stdout)
            } else {
                Err(std::io::Error::other("hostname failed"))
            }
        })
        .map_err(|e| format!("can't identify this machine: {e}"))?;
    if machine.is_empty() {
        return Err("can't identify this machine: empty identity".into());
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(value!({
        "machine_sha256": crate::sha256_hex(&machine), "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH, "adapter": host.adapter(),
        "logical_cpus": std::thread::available_parallelism().map_err(|e| e.to_string())?.get(),
        "host_sha256": crate::sha256_hex(&std::fs::read(executable).map_err(|e| e.to_string())?),
    }))
}

pub(super) fn workload(a: &Args, script: &[Scripted]) -> Value {
    value!({ "scenario": a.scenario, "frames": a.frames, "warmup": a.warmup,
        "size": [a.size.0, a.size.1], "fps": 60, "serial": a.serial, "workers": a.workers,
        "input_sha256": crate::sha256_hex(format!("{script:?}").as_bytes()) })
}

pub(super) fn compatible(before: &Value, after: &Value, what: &str) -> Result<(), String> {
    if before == after {
        Ok(())
    } else {
        Err(format!(
            "incompatible {what}; compare reports with identical hardware and replay settings"
        ))
    }
}

pub(super) fn compatible_hardware(before: &Value, after: &Value) -> Result<(), String> {
    // A different host binary can be the intended optimization. Record it, but compare
    // hardware and workload independently of the code under measurement.
    let hardware = |v: &Value| {
        let mut v = v.clone();
        if let Some(fields) = v.as_object_mut() {
            fields.remove("host_sha256");
        }
        v
    };
    compatible(&hardware(before), &hardware(after), "machine, GPU or driver")
}

/// Hash the manifest, WASM, shaders and packaged files, with their names and lengths.
pub(super) fn build_hash(dir: &Path) -> Result<String, String> {
    let manifest = std::fs::read(dir.join("manifest.json")).map_err(|e| e.to_string())?;
    let m = wrela_abi::Manifest::parse(std::str::from_utf8(&manifest).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut paths = vec![PathBuf::from("manifest.json"), PathBuf::from(m.wasm)];
    paths.extend(m.pipelines.into_iter().map(|p| PathBuf::from(p.shader)));
    fn files(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        for entry in std::fs::read_dir(root.join(rel)).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let next = rel.join(entry.file_name());
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                files(root, &next, out)?;
            } else if kind.is_file() {
                out.push(next);
            } else {
                return Err(format!("benchmark assets must be regular files: {}", next.display()));
            }
        }
        Ok(())
    }
    if dir.join("files").exists() {
        files(dir, Path::new("files"), &mut paths)?;
    }
    paths.sort();
    paths.dedup();
    let mut hash = Sha256::new();
    for path in paths {
        let name = path.to_string_lossy();
        let bytes = std::fs::read(dir.join(&path)).map_err(|e| e.to_string())?;
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// AB, BA, AB... keeps a consistently first or second sample from favoring one build.
fn order(pair: u32) -> [usize; 2] {
    if pair.is_multiple_of(2) { [0, 1] } else { [1, 0] }
}

fn stats(samples: &[f64]) -> Value {
    value!({ "median_ms": median(samples.to_vec()),
        "min_ms": samples.iter().copied().reduce(f64::min),
        "max_ms": samples.iter().copied().reduce(f64::max) })
}

fn metric(before: &[f64], after: &[f64]) -> Value {
    let paired: Vec<f64> = before
        .iter()
        .zip(after)
        .filter_map(|(&b, &a)| {
            (before.len() == after.len() && b > 0.0).then_some((a / b - 1.0) * 100.0)
        })
        .collect();
    let status = if before.is_empty() && after.is_empty() {
        "unavailable"
    } else if before.is_empty() {
        "added"
    } else if after.is_empty() {
        "removed"
    } else if before.len() != after.len() {
        "inconclusive"
    } else if after.iter().copied().reduce(f64::max) < before.iter().copied().reduce(f64::min) {
        "faster"
    } else if after.iter().copied().reduce(f64::min) > before.iter().copied().reduce(f64::max) {
        "slower"
    } else {
        "inconclusive"
    };
    value!({ "baseline": stats(before), "candidate": stats(after), "status": status,
        "paired_change_percent": paired, "median_change_percent": median(paired) })
}

pub(super) fn run(
    a: &Args,
    host: &mut Host,
    options: &Options,
    script: &[Scripted],
    built: &Path,
    environment: &Value,
    workload: &Value,
) -> Result<String, String> {
    let before = a.compare.as_deref().expect("comparison baseline");
    let dirs = [before, built];
    let hashes = [build_hash(before)?, build_hash(built)?];
    let mut samples: [Vec<Profile>; 2] = [Vec::new(), Vec::new()];
    let mut sequence = Vec::new();
    for pair in 0..a.pairs {
        for which in order(pair) {
            host.restart_in(dirs[which], options).map_err(|e| e.to_string())?;
            // Restart has joined the old program's workers. Each deferred init sees empty storage.
            let storage = options.storage.as_ref().expect("isolated profile storage");
            std::fs::remove_dir_all(storage).map_err(|e| e.to_string())?;
            std::fs::create_dir(storage).map_err(|e| e.to_string())?;
            let p = measure(a, host, script, dirs[which], environment, workload)?;
            if p.build_sha256 != hashes[which] {
                return Err("build changed during measurement; discard this run".into());
            }
            samples[which].push(p);
            sequence.push(if which == 0 { "baseline" } else { "candidate" });
        }
    }
    for (dir, hash) in dirs.iter().zip(&hashes) {
        if build_hash(dir)? != *hash {
            return Err("build changed during measurement; discard this run".into());
        }
    }
    let spans = |side: usize| {
        samples[side]
            .iter()
            .filter_map(|p| p.span.filter(|_| !a.serial).map(|s| s.median))
            .collect::<Vec<_>>()
    };
    let calls = |side: usize| samples[side].iter().map(|p| p.frame.median).collect::<Vec<_>>();
    let names: std::collections::BTreeSet<_> =
        samples.iter().flatten().flat_map(|p| p.labels.iter().map(|l| l.name.clone())).collect();
    let labels: Vec<_> = names
        .into_iter()
        .map(|name| {
            let times = |side: usize| {
                samples[side]
                    .iter()
                    .filter_map(|p| p.labels.iter().find(|l| l.name == name).map(|l| l.median))
                    .collect::<Vec<_>>()
            };
            let (before, after) = (times(0), times(1));
            let mut comparison = metric(&before, &after);
            if (!before.is_empty() && before.len() != a.pairs as usize)
                || (!after.is_empty() && after.len() != a.pairs as usize)
            {
                comparison["status"] = "inconclusive".into();
                comparison["paired_change_percent"] = value!([]);
                comparison["median_change_percent"] = Value::Null;
            }
            value!({ "label": name, "comparison": comparison })
        })
        .collect();
    let sample_reports: Vec<Vec<Value>> = samples
        .iter()
        .map(|side| {
            side.iter()
                .map(|p| serde_json::from_str(&json(a, p, None)).expect("profile JSON"))
                .collect()
        })
        .collect();
    let report = value!({ "schema": 2, "environment": environment, "workload": workload,
        "pairs": a.pairs, "order": sequence, "baseline_build_sha256": hashes[0],
        "candidate_build_sha256": hashes[1], "gpu_span": metric(&spans(0), &spans(1)),
        "cpu_frame_call": metric(&calls(0), &calls(1)), "labels": labels,
        "samples": { "baseline": sample_reports[0], "candidate": sample_reports[1] } });
    if a.json {
        return Ok(format!("{report}\n"));
    }
    let mut out = format!(
        "{}: {} alternating pairs; {} warmup + {} measured frames, {}×{}, {} workers\n{}\n",
        a.scenario,
        a.pairs,
        a.warmup,
        a.frames,
        a.size.0,
        a.size.1,
        a.workers,
        environment["adapter"].as_str().unwrap_or("")
    );
    fn line(name: &str, m: &Value) -> String {
        let show = |side: &str| match m[side]["median_ms"].as_f64() {
            Some(ms) => format!(
                "{ms:.3} ms [{:.3}, {:.3}]",
                m[side]["min_ms"].as_f64().unwrap(),
                m[side]["max_ms"].as_f64().unwrap()
            ),
            None => "absent".into(),
        };
        let change = m["median_change_percent"]
            .as_f64()
            .map(|v| format!("{v:+.1}%"))
            .unwrap_or_else(|| "n/a".into());
        format!(
            "{name}: {} -> {}; {change}, {}\n",
            show("baseline"),
            show("candidate"),
            m["status"].as_str().unwrap()
        )
    }
    if !a.serial {
        out.push_str(&line("GPU frame span", &report["gpu_span"]));
    }
    out.push_str(&line("CPU frame call (includes GPU waits)", &report["cpu_frame_call"]));
    for l in report["labels"].as_array().unwrap() {
        out.push_str(&line(l["label"].as_str().unwrap(), &l["comparison"]));
    }
    out.push_str("Brackets show the range of run medians. Overlapping ranges are inconclusive, not a confidence interval.\nTimings describe this GPU and instrumented mode, not browser FPS. Use --json to retain every sample.\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparisons_report_noise_zero_cost_and_missing_work_without_a_timing_gate() {
        assert_eq!(metric(&[10.0, 12.0, 11.0], &[8.0, 9.0, 8.5])["status"], "faster");
        assert_eq!(metric(&[10.0, 12.0, 11.0], &[11.0, 13.0, 12.0])["status"], "inconclusive");
        assert_eq!(metric(&[1.0, 1.0, 1.0], &[2.0, 2.0, 2.0])["median_change_percent"], 100.0);
        assert_eq!(metric(&[0.0], &[0.0])["median_change_percent"], Value::Null);
        assert_eq!(metric(&[1.0], &[])["status"], "removed");
        assert_eq!(metric(&[], &[1.0])["status"], "added");
        assert_eq!(order(0), [0, 1]);
        assert_eq!(order(1), [1, 0]);
        assert_eq!(order(2), [0, 1]);
    }

    #[test]
    fn build_identity_covers_packaged_files_and_storage_is_disposable() {
        let scratch = Storage::new().unwrap();
        let root = scratch.0.clone();
        std::fs::write(
            root.join("manifest.json"),
            r#"{"manifest_version":9,"stream_version":11,"wasm":"game.wasm","pipelines":[]}"#,
        )
        .unwrap();
        std::fs::write(root.join("game.wasm"), b"first program").unwrap();
        std::fs::create_dir(root.join("files")).unwrap();
        std::fs::write(root.join("files/scene"), b"first scene").unwrap();
        let first = build_hash(&root).unwrap();
        assert_eq!(build_hash(&root).unwrap(), first);
        std::fs::write(root.join("files/scene"), b"other scene").unwrap();
        let changed = build_hash(&root).unwrap();
        assert_ne!(changed, first);
        std::fs::write(root.join("game.wasm"), b"other program").unwrap();
        assert_ne!(build_hash(&root).unwrap(), changed);
        drop(scratch);
        assert!(!root.exists());
    }

    #[test]
    fn measurement_leases_exclude_other_profilers_and_release_on_drop() {
        let path =
            std::env::temp_dir().join(format!("wrela-profile-lock-test-{}", std::process::id()));
        let first = lease(&path).unwrap();
        assert!(lease(&path).is_err());
        drop(first);
        drop(lease(&path).unwrap());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn hardware_and_workload_must_match_including_input_and_warmup() {
        let hardware =
            value!({"machine_sha256": "one", "adapter": "GPU / driver", "host_sha256": "old"});
        let mut changed = hardware.clone();
        changed["host_sha256"] = "new".into();
        assert!(compatible_hardware(&hardware, &changed).is_ok());
        changed["adapter"] = "other driver".into();
        assert!(compatible_hardware(&hardware, &changed).is_err());
        assert!(parse(&[".".into()]).is_some());
        let a = parse(&["scene".into()]).unwrap();
        let w = workload(&a, &[]);
        assert!(compatible(&w, &w, "workload").is_ok());
        for field in
            ["scenario", "frames", "warmup", "size", "fps", "serial", "workers", "input_sha256"]
        {
            let mut other = w.clone();
            other[field] = Value::Null;
            assert!(compatible(&w, &other, "workload").is_err(), "{field}");
        }
        assert!(parse(&["scene".into(), "--warmup".into(), u32::MAX.to_string()]).is_none());
        assert!(
            parse(&[
                "scene".into(),
                "--compare".into(),
                "build".into(),
                "--pairs".into(),
                "2".into()
            ])
            .is_none()
        );
    }
}
