//! `wrela build <package-dir> [-o <out-dir>] [--debug] [--testing] [--optimize [size|speed]] [--lift <package>]... [--json]`: checks and builds a package, writing
//! `game.wasm`, the WGSL, `manifest.json` and the runtime into the output directory (default
//! `<package>/build`). With `--optimize`, `game.wasm` goes through Binaryen (for size, `-Oz`, or
//! speed, `-O3`; alone, `wrela_driver::optimize::Level::DEFAULT`) and its trap
//! locations are written beside it, as `game.lines` (`wrela_driver::optimize`). The output is the same bytes for the same input (AC9). A file an earlier
//! build wrote there and this one doesn't (every one, when the build fails) is removed; other
//! files are left alone, and the build stops rather than replace one. `.wrela-build` there
//! lists the files the last build wrote.

use std::collections::HashSet;
use std::path::Path;
use std::process::ExitCode;

pub fn run(args: &[String]) -> ExitCode {
    let args = match crate::package_args(args, true) {
        Ok(a) => a,
        Err(status) => return status,
    };
    let dir = &args.dir;
    let out = args.out.unwrap_or_else(|| dir.join("build"));
    let mut output = if !args.lift.is_empty() {
        let kind = wrela_driver::BuildKind { debug: args.debug, testing: args.testing };
        match wrela_driver::build_lifted(dir, &args.lift, kind) {
            Ok(o) => o,
            Err(why) => {
                eprintln!("error: {why}");
                return ExitCode::from(2);
            }
        }
    } else if args.testing {
        wrela_driver::build_for_tests(dir, args.debug)
    } else if args.debug {
        wrela_driver::build_debug(dir)
    } else if args.relaxed_simd {
        wrela_driver::build_relaxed(dir)
    } else {
        wrela_driver::build(dir)
    };
    let failed = crate::report(&output, args.json);
    if args.report_data
        && let Some(data) = &output.data
    {
        print!("{}", if args.json { data_json(data) } else { data_table(data) });
    }
    if args.report_consts {
        let runs = &output.consts.runs;
        print!("{}", if args.json { consts_json(runs) } else { consts_table(runs) });
    }
    if let Some(level) = args.optimize
        && !failed
        && let Err(e) = optimized(&mut output.files, level)
    {
        eprintln!("error: {e}");
        return ExitCode::from(2);
    }
    // Before anything in `out` is removed or written.
    if let Err(e) = symlinks_in_way(&out, &output.files) {
        eprintln!("error: {e}");
        return ExitCode::from(2);
    }
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
    let owned_names: HashSet<&str> = owned.iter().map(String::as_str).collect();
    if let Some((name, _)) =
        output.files.iter().find(|(f, _)| !owned_names.contains(f.as_str()) && out.join(f).exists())
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
                let c = output.consts;
                let consts = if c.computed + c.cached == 0 {
                    String::new()
                } else {
                    format!("; constants: {} computed, {} from the cache", c.computed, c.cached)
                };
                eprintln!("built {} ({} files{consts})", out.display(), output.files.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: can't write `{}`: {e}", out.display());
            ExitCode::from(2)
        }
    }
}

/// `--report consts`' table: each computed constant, its value's bytes, the fuel computing it
/// used, this build's seconds on it, and whether it was read from the cache.
pub(crate) fn consts_table(runs: &[wrela_driver::ConstRun]) -> String {
    let mut s =
        format!("{:<40} {:>12} {:>16} {:>9}  {}\n", "constant", "bytes", "fuel", "seconds", "from");
    for r in runs {
        let from = if r.cached { "cached" } else { "computed" };
        s.push_str(&format!(
            "{:<40} {:>12} {:>16} {:>9.3}  {from}\n",
            r.name, r.bytes, r.fuel, r.seconds
        ));
    }
    let cached = runs.iter().filter(|r| r.cached).count();
    s.push_str(&format!(
        "consts: {} computed, {cached} from the cache, {:.3} s\n",
        runs.len() - cached,
        runs.iter().map(|r| r.seconds).sum::<f64>()
    ));
    s
}

/// `--report consts --json`.
fn consts_json(runs: &[wrela_driver::ConstRun]) -> String {
    let rows: Vec<_> = runs
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.name,
                "bytes": r.bytes,
                "fuel": r.fuel,
                "seconds": r.seconds,
                "cached": r.cached,
            })
        })
        .collect();
    format!("{}\n", serde_json::json!({ "consts": rows }))
}

/// How many of the largest constants `--report data` lists.
const LARGEST: usize = 20;

/// `--report data`'s table: the largest constants, the section's total and its zero share.
fn data_table(d: &wrela_driver::DataReport) -> String {
    let share =
        |zeros: u32, of: u32| if of == 0 { 0.0 } else { 100.0 * f64::from(zeros) / f64::from(of) };
    let mut s = format!("{:<40} {:<32} {:>12} {:>7}\n", "constant", "type", "bytes", "zeros");
    for (name, ty, bytes, zeros) in d.constants.iter().take(LARGEST) {
        let ty = if ty.len() > 32 {
            format!("{}…", &ty[..ty.floor_char_boundary(31)])
        } else {
            ty.clone()
        };
        s.push_str(&format!("{name:<40} {ty:<32} {bytes:>12} {:>6.1}%\n", share(*zeros, *bytes)));
    }
    s.push_str(&format!(
        "data: {} bytes in {} constants, {:.1}% zero; the segments hold {} bytes\n",
        d.total,
        d.constants.len(),
        share(d.zeros, d.total),
        d.segments
    ));
    s
}

/// `--report data --json`.
fn data_json(d: &wrela_driver::DataReport) -> String {
    let constants: Vec<_> = d
        .constants
        .iter()
        .take(LARGEST)
        .map(|(name, ty, bytes, zeros)| {
            serde_json::json!({ "name": name, "type": ty, "bytes": bytes, "zero_bytes": zeros })
        })
        .collect();
    let all = serde_json::json!({
        "data": {
            "bytes": d.total,
            "zero_bytes": d.zeros,
            "segment_bytes": d.segments,
            "constants": d.constants.len(),
            "largest": constants,
        }
    });
    format!("{all}\n")
}

/// The build's files with `game.wasm` optimized, and its trap locations beside it.
pub(crate) fn optimized(
    files: &mut Vec<(String, Vec<u8>)>,
    level: wrela_driver::optimize::Level,
) -> Result<(), String> {
    optimized_with(files, |wasm, relaxed| wrela_driver::optimize::optimize(wasm, level, relaxed))
}

/// [`optimized`], with `optimize` making the optimized code and its table.
pub(crate) fn optimized_with(
    files: &mut Vec<(String, Vec<u8>)>,
    mut optimize: impl FnMut(&[u8], bool) -> Result<(Vec<u8>, Vec<u8>), String>,
) -> Result<(), String> {
    let wasm =
        files.iter_mut().find(|(f, _)| f == "game.wasm").ok_or("the build has no game.wasm")?;
    let (code, lines) = optimize(&wasm.1, false)?;
    wasm.1 = code;
    files.push((LINES.to_string(), lines));
    // A relaxed module, through `wasm-opt` with relaxed SIMD allowed (its own trap lines: the
    // relaxed module's offsets are its own).
    if let Some(relaxed) = files.iter_mut().find(|(f, _)| f == wrela_driver::build::RELAXED_WASM) {
        let (code, lines) = optimize(&relaxed.1, true)?;
        relaxed.1 = code;
        files.push((RELAXED_LINES.to_string(), lines));
    }
    wrela_driver::build::restate_abi(files)
}

/// An optimized relaxed module's trap locations, beside it.
const RELAXED_LINES: &str = "game.relaxed.lines";

/// An optimized build's trap locations, beside `game.wasm` (`wrela_abi::lines::SIDECAR`).
const LINES: &str = "game.lines";

/// Whether a build writes a file with this name: `game.wasm` (and a relaxed-SIMD build's
/// `game.relaxed.wasm`), their trap locations, `manifest.json`, a pipeline's
/// `pipeline_<n>.wgsl`, a file of the runtime, a server build's `server/game.wasm` or a tool
/// build's `tool/tool.wasm`.
pub(crate) fn written_by_build(name: &str) -> bool {
    let pipeline = name
        .strip_prefix("pipeline_")
        .and_then(|n| n.strip_suffix(".wgsl"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    // A shipped constant's files (`std::io::Shipped`): `files/<dir>/<name>`.
    let shipped = name.strip_prefix("files/").is_some_and(|rest| {
        let parts: Vec<&str> = rest.split('/').collect();
        parts.len() == 2 && parts.iter().all(|p| !p.is_empty() && *p != "." && *p != "..")
    });
    pipeline
        || shipped
        || name == "game.wasm"
        || name == LINES
        || name == wrela_driver::build::RELAXED_WASM
        || name == RELAXED_LINES
        || name == "manifest.json"
        || name == "lift.json"
        || name == wrela_driver::build::SERVER_WASM
        || name == wrela_driver::build::TOOL_WASM
        || wrela_driver::build::RUNTIME_FILES.iter().any(|(f, _)| *f == name)
}

/// The file in the output directory that lists the files the last build wrote there: the tool
/// host's toolchain service keeps it by the same rules (`crate::toolchain`).
const LIST: &str = wrela_host::tool::toolchain::LIST;

/// A build output (or its ownership list) that's a symbolic link. Writing a symlink would
/// replace its target, which isn't a file this build owns.
fn symlinks_in_way(out: &Path, files: &[(String, Vec<u8>)]) -> std::io::Result<()> {
    let names = files.iter().map(|(name, _)| name.as_str()).chain(std::iter::once(LIST));
    wrela_driver::ensure_no_symlinks(out, names)
}

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
    let current: HashSet<&str> = files.iter().map(|(f, _)| f.as_str()).collect();
    let stale: Vec<&String> =
        owned.iter().filter(|name| !current.contains(name.as_str())).collect();
    wrela_driver::ensure_no_symlinks(out, stale.iter().map(|name| name.as_str()))?;
    for name in stale {
        let path = out.join(name);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--report consts` names each constant, and a cached read as cached.
    #[test]
    fn the_consts_report_says_which_were_cached() {
        let run = |name: &str, cached| wrela_driver::ConstRun {
            name: name.into(),
            bytes: 256,
            fuel: 1_000,
            seconds: 0.25,
            cached,
        };
        let table = consts_table(&[run("SQUARES", false), run("TOTAL", true)]);
        let lines: Vec<&str> = table.lines().collect();
        assert!(lines[1].starts_with("SQUARES") && lines[1].ends_with("computed"), "{table}");
        assert!(lines[2].starts_with("TOTAL") && lines[2].ends_with("cached"), "{table}");
        assert_eq!(lines[3], "consts: 1 computed, 1 from the cache, 0.500 s");
        let json = consts_json(&[run("TOTAL", true)]);
        assert!(json.contains("\"cached\":true") && json.contains("\"fuel\":1000"), "{json}");
    }

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

    #[cfg(unix)]
    #[test]
    fn a_build_doesnt_write_through_an_owned_symlink() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!("wrela-cli-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("victim"), "keep").expect("write victim");
        symlink("victim", dir.join("game.wasm")).expect("symlink");
        let files = [("game.wasm".to_string(), Vec::new())];
        let refused = symlinks_in_way(&dir, &files).expect_err("a symbolic link");
        assert!(refused.to_string().contains("game.wasm"), "{refused}");
        assert_eq!(std::fs::read_to_string(dir.join("victim")).unwrap(), "keep");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn stale_files_arent_removed_through_a_linked_parent() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!("wrela-cli-stale-link-{}", std::process::id()));
        let outside =
            std::env::temp_dir().join(format!("wrela-cli-stale-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(dir.join("files")).expect("output");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(outside.join("small.bin"), "keep").expect("victim");
        symlink(&outside, dir.join("files/data")).expect("symlink");
        let owned = vec!["files/data/small.bin".to_string()];
        let refused = remove_stale(&dir, &owned, &[]).expect_err("a symbolic-link parent");
        assert!(refused.to_string().contains("files/data"), "{refused}");
        assert_eq!(std::fs::read_to_string(outside.join("small.bin")).unwrap(), "keep");
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(outside);
    }
}
