//! `wrela build <package-dir> [-o <out-dir>] [--json]`: checks and builds a package, writing
//! `game.wasm`, the WGSL, `manifest.json` and the runtime into the output directory (default
//! `<package>/build`). The output is the same bytes for the same input (AC9). A file an earlier
//! build wrote there and this one doesn't (every one, when the build fails) is removed; other
//! files are left alone.

use std::path::Path;
use std::process::ExitCode;

pub fn run(args: &[String]) -> ExitCode {
    let args = match crate::package_args(args, true) {
        Ok(a) => a,
        Err(status) => return status,
    };
    let dir = &args.dir;
    let out = args.out.unwrap_or_else(|| dir.join("build"));
    let output = wrela_driver::build(dir);
    let failed = crate::report(&output, args.json);
    // The files an earlier build wrote and this one doesn't (all of them, if it failed) would
    // be served with this build's: they go.
    if let Err(e) = remove_stale(&out, &output.files) {
        eprintln!("error: can't clear `{}`: {e}", out.display());
        return ExitCode::from(2);
    }
    if failed {
        return ExitCode::from(1);
    }
    match output.write_to(&out) {
        Ok(()) => {
            if !args.json {
                eprintln!("built {} ({} files)", out.display(), output.files.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: can't write `{}`: {e}", out.display());
            ExitCode::from(2)
        }
    }
}

/// Whether a build writes a file with this name: `game.wasm`, `manifest.json`, a pipeline's
/// `pipeline_<n>.wgsl` or a file of the runtime.
fn written_by_build(name: &str) -> bool {
    let pipeline = name
        .strip_prefix("pipeline_")
        .and_then(|n| n.strip_suffix(".wgsl"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    pipeline
        || name == "game.wasm"
        || name == "manifest.json"
        || wrela_driver::build::RUNTIME_FILES.iter().any(|(f, _)| *f == name)
}

/// Removes the files in `out` that a build writes but `files` doesn't hold. Nothing else in
/// `out` is touched.
fn remove_stale(out: &Path, files: &[(String, Vec<u8>)]) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(out) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if written_by_build(&name)
            && entry.file_type()?.is_file()
            && !files.iter().any(|(f, _)| *f == name)
        {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_a_build_writes_are_stale() {
        let dir = std::env::temp_dir().join(format!("wrela-cli-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        for f in ["pipeline_0.wgsl", "pipeline_1.wgsl", "pipeline_x.wgsl", "game.wasm", "notes.txt"]
        {
            std::fs::write(dir.join(f), "old").expect("write");
        }
        let files = vec![("pipeline_0.wgsl".to_string(), Vec::new())];
        remove_stale(&dir, &files).expect("remove");
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .expect("read")
            .map(|e| e.expect("entry").file_name().to_string_lossy().to_string())
            .collect();
        left.sort();
        assert_eq!(left, ["notes.txt", "pipeline_0.wgsl", "pipeline_x.wgsl"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
