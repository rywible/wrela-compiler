//! `wrela build <package-dir> [-o <out-dir>] [--json]`: checks and builds a package, writing
//! `game.wasm`, the WGSL, `manifest.json` and the runtime into the output directory (default
//! `<package>/build`). The output is the same bytes for the same input (AC9).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub fn run(args: &[String]) -> ExitCode {
    let mut dir = None;
    let mut out = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--out" => match it.next() {
                Some(o) => out = Some(PathBuf::from(o)),
                None => return crate::usage(),
            },
            "--json" => json = true,
            _ if dir.is_none() && !a.starts_with('-') => dir = Some(PathBuf::from(a)),
            _ => return crate::usage(),
        }
    }
    let Some(dir) = dir else { return crate::usage() };
    if !dir.is_dir() {
        eprintln!(
            "error: `{}` isn't a directory (a package is a directory of .wrela files)",
            dir.display()
        );
        return ExitCode::from(2);
    }
    if !dir.join("main.wrela").is_file() {
        eprintln!("error: `{}` has no main.wrela, so there's no program to build", dir.display());
        return ExitCode::from(2);
    }
    let out = out.unwrap_or_else(|| dir.join("build"));
    let compiler = wrela_driver::Compiler::new(&dir);
    let (check, diags, files) = compiler.build();
    let report = wrela_driver::CheckOutput {
        sources: clone_sources(&check.sources),
        diagnostics: diags,
        checked: None,
    };
    let failed = crate::report(&report, json);
    if failed {
        return ExitCode::from(1);
    }
    match write(&out, &files.files) {
        Ok(()) => {
            if !json {
                eprintln!("built {} ({} files)", out.display(), files.files.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: can't write `{}`: {e}", out.display());
            ExitCode::from(2)
        }
    }
}

fn clone_sources(map: &wrela_diag::SourceMap) -> wrela_diag::SourceMap {
    let mut out = wrela_diag::SourceMap::new();
    for (_, f) in map.files() {
        out.add(f.name.clone(), f.text.clone());
    }
    out
}

fn write(out: &Path, files: &[(String, Vec<u8>)]) -> std::io::Result<()> {
    std::fs::create_dir_all(out)?;
    for (path, bytes) in files {
        let p = out.join(path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(p, bytes)?;
    }
    Ok(())
}
