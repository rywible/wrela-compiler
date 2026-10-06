//! `wrela build <package-dir> [-o <out-dir>] [--debug] [--json]`: checks and builds a package, writing
//! `game.wasm`, the WGSL, `manifest.json` and the runtime into the output directory (default
//! `<package>/build`). The output is the same bytes for the same input (AC9). A file an earlier
//! build wrote there and this one doesn't (every one, when the build fails) is removed; other
//! files are left alone, and the build stops rather than replace one. `.wrela-build` there
//! lists the files the last build wrote.

use std::path::Path;
use std::process::ExitCode;

pub fn run(args: &[String]) -> ExitCode {
    let args = match crate::package_args(args, true) {
        Ok(a) => a,
        Err(status) => return status,
    };
    let dir = &args.dir;
    let out = args.out.unwrap_or_else(|| dir.join("build"));
    let output = if !args.lift.is_empty() {
        match wrela_driver::build_lifted(dir, &args.lift, args.debug) {
            Ok(o) => o,
            Err(why) => {
                eprintln!("error: {why}");
                return ExitCode::from(2);
            }
        }
    } else if args.debug {
        wrela_driver::build_debug(dir)
    } else {
        wrela_driver::build(dir)
    };
    let failed = crate::report(&output, args.json);
    let owned = match owned(&out) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: can't read `{}`: {e}", out.display());
            return ExitCode::from(2);
        }
    };
    // The files an earlier build wrote and this one doesn't (all of them, if it failed) would
    // be served with this build's: they go.
    if let Err(e) = remove_stale(&out, &owned, &output.files) {
        eprintln!("error: can't clear `{}`: {e}", out.display());
        return ExitCode::from(2);
    }
    if failed {
        return ExitCode::from(1);
    }
    if let Some((name, _)) =
        output.files.iter().find(|(f, _)| !owned.contains(f) && out.join(f).exists())
    {
        eprintln!(
            "error: `{}` is in the way: an earlier `wrela build` didn't write it, so it isn't replaced",
            out.join(name).display()
        );
        eprintln!("  = help: move it, or build into another directory with `-o`");
        return ExitCode::from(2);
    }
    let list: String = output.files.iter().map(|(f, _)| format!("{f}\n")).collect();
    let written = output.write_to(&out).and_then(|()| std::fs::write(out.join(LIST), list));
    match written {
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
        || name == "lift.json"
        || wrela_driver::build::RUNTIME_FILES.iter().any(|(f, _)| *f == name)
}

/// The file in the output directory that lists the files the last build wrote there.
const LIST: &str = ".wrela-build";

/// The files in `out` that an earlier build wrote: those [`LIST`] names, or, in a directory a
/// build wrote before there was a list (it has `game.wasm` and `manifest.json`), every file a
/// build writes. Any other file is the user's.
fn owned(out: &Path) -> std::io::Result<Vec<String>> {
    let names = |out: &Path| -> std::io::Result<Vec<String>> {
        match std::fs::read_dir(out) {
            Ok(entries) => {
                entries.map(|e| e.map(|e| e.file_name().to_string_lossy().to_string())).collect()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    };
    match std::fs::read_to_string(out.join(LIST)) {
        Ok(list) => Ok(list.lines().filter(|l| written_by_build(l)).map(String::from).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let present = names(out)?;
            let old_build =
                ["game.wasm", "manifest.json"].iter().all(|f| present.iter().any(|p| p == f));
            Ok(if old_build {
                present.into_iter().filter(|n| written_by_build(n)).collect()
            } else {
                Vec::new()
            })
        }
        Err(e) => Err(e),
    }
}

/// Removes the files in `out` that an earlier build wrote (`owned`) but `files` doesn't hold.
/// Nothing else in `out` is touched.
fn remove_stale(out: &Path, owned: &[String], files: &[(String, Vec<u8>)]) -> std::io::Result<()> {
    for name in owned {
        let path = out.join(name);
        if !files.iter().any(|(f, _)| f == name)
            && std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file())
        {
            std::fs::remove_file(path)?;
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
        // A build wrote these before there was a list: every file a build writes is stale.
        std::fs::write(dir.join("manifest.json"), "old").expect("write");
        let owned = owned(&dir).expect("owned");
        remove_stale(&dir, &owned, &files).expect("remove");
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .expect("read")
            .map(|e| e.expect("entry").file_name().to_string_lossy().to_string())
            .collect();
        left.sort();
        assert_eq!(left, ["notes.txt", "pipeline_0.wgsl", "pipeline_x.wgsl"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_build_owns_only_what_it_wrote() {
        let dir = std::env::temp_dir().join(format!("wrela-cli-owned-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        // The user's page, with names a build writes: not a build's, so not stale.
        for f in ["index.html", "main.js"] {
            std::fs::write(dir.join(f), "mine").expect("write");
        }
        assert!(owned(&dir).expect("owned").is_empty());
        // The list names what the last build wrote, and nothing else counts.
        std::fs::write(dir.join(LIST), "game.wasm\npipeline_0.wgsl\nnotes.txt\n").expect("write");
        assert_eq!(owned(&dir).expect("owned"), ["game.wasm", "pipeline_0.wgsl"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
