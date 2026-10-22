//! Finding a package's files (Q1 of #6): one package, a file is a module, a directory is a
//! module tree. `shapes/blob.wrela` is the module `shapes::blob`; `main.wrela` is the entry.
//!
//! Symbolic links to directories are refused, loudly (the open question on #6): following them
//! needs canonical-path dedup and cycle protection, and until that's decided a check must never
//! pass without reading the files behind a link.

use crate::reach::{Kind, Reach};
use std::path::{Path, PathBuf};
use wrela_syntax::lexer::is_name;

/// A source file of the package and the module it is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PackageFile {
    pub module: Vec<String>,
    pub path: PathBuf,
    /// The path shown in diagnostics: relative to the package root.
    pub display: String,
}

/// A problem with the package's layout, before any file is parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutError {
    pub path: String,
    pub message: String,
    pub help: Option<String>,
    /// A symbolic link to a directory (E0208), rather than a layout or read problem (E0205).
    pub symlink: bool,
}

pub fn find_files(root: &Path) -> Result<Vec<PackageFile>, Vec<LayoutError>> {
    find_files_in(&Reach::anywhere(), root)
}

/// [`find_files`], reading only what `reach` may (§15.5's toolchain service).
pub fn find_files_in(reach: &Reach, root: &Path) -> Result<Vec<PackageFile>, Vec<LayoutError>> {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    walk(reach, root, root, &mut Vec::new(), &mut files, &mut errors);
    files.sort_by(|a, b| a.module.cmp(&b.module));
    if errors.is_empty() { Ok(files) } else { Err(errors) }
}

fn walk(
    reach: &Reach,
    root: &Path,
    dir: &Path,
    prefix: &mut Vec<String>,
    files: &mut Vec<PackageFile>,
    errors: &mut Vec<LayoutError>,
) {
    let display = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
    let mut entries = match reach.entries(dir) {
        Ok(e) => e,
        Err(e) => {
            errors.push(LayoutError {
                path: display(dir),
                message: format!("can't read the directory: {e}"),
                help: None,
                symlink: false,
            });
            return;
        }
    };
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (file_name, kind) in entries {
        let path = dir.join(&file_name);
        let name = file_name.to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        // Skipped whatever they are: a link to an output directory elsewhere is no module.
        if prefix.is_empty() && wrela_sema::OUTPUT_DIRS.contains(&name.as_str()) {
            continue;
        }
        // A link's target, followed: an error is a link out of the roots, or a broken one.
        let target = if kind == Kind::Link { reach.kind(&path, true) } else { Ok(Some(kind)) };
        if kind == Kind::Link {
            if matches!(target, Ok(Some(Kind::Dir))) {
                errors.push(LayoutError {
                    path: display(&path),
                    message: format!(
                        "`{}` is a symbolic link to a directory, which wrela doesn't follow",
                        display(&path)
                    ),
                    help: Some(
                        "check its target directly, or replace the link with the directory".into(),
                    ),
                    symlink: true,
                });
                continue;
            }
        } else if kind == Kind::Dir {
            // A directory with a manifest is another package, not part of this one's tree.
            if reach.is_file(&path.join("wrela.toml")) {
                continue;
            }
            if !is_name(&name) {
                if has_wrela_files(reach, &path) {
                    errors.push(LayoutError {
                        path: display(&path),
                        message: format!(
                            "the directory `{name}` holds modules, so its name must be a wrela name"
                        ),
                        help: Some("rename it with letters, digits and `_`".into()),
                        symlink: false,
                    });
                }
                continue;
            }
            prefix.push(name);
            walk(reach, root, &path, prefix, files, errors);
            prefix.pop();
            continue;
        }
        let Some(stem) = name.strip_suffix(".wrela") else { continue };
        // A pipe or a device would block or never end when read.
        let regular = matches!(target, Ok(Some(Kind::File)));
        if !regular {
            errors.push(LayoutError {
                path: display(&path),
                message: format!("`{name}` isn't a regular file, so it can't be a module"),
                help: None,
                symlink: false,
            });
            continue;
        }
        if !is_name(stem) {
            errors.push(LayoutError {
                path: display(&path),
                message: format!("`{name}` can't be a module: `{stem}` isn't a wrela name"),
                help: Some(
                    "rename it with letters, digits and `_`, not starting with a digit".into(),
                ),
                symlink: false,
            });
            continue;
        }
        let mut module = prefix.clone();
        module.push(stem.to_string());
        files.push(PackageFile { module, display: display(&path), path });
    }
}

/// Whether `dir` holds a `.wrela` file, in it or below it, as [`walk`] would find one: hidden
/// entries and other packages are skipped, and symbolic links aren't followed (so a cycle of
/// links ends).
fn has_wrela_files(reach: &Reach, dir: &Path) -> bool {
    if reach.is_file(&dir.join("wrela.toml")) {
        return false;
    }
    reach.entries(dir).is_ok_and(|entries| {
        entries.into_iter().any(|(name, kind)| {
            let p = dir.join(&name);
            if name.to_string_lossy().starts_with('.') {
                return false;
            }
            p.extension().is_some_and(|x| x == "wrela")
                || (kind == Kind::Dir && has_wrela_files(reach, &p))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wrela-pkg-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("temp dir");
        d
    }

    #[test]
    fn files_and_directories_are_modules() {
        let d = tmp("tree");
        fs::write(d.join("main.wrela"), "").expect("write");
        fs::create_dir_all(d.join("shapes")).expect("mkdir");
        fs::write(d.join("shapes/blob.wrela"), "").expect("write");
        fs::create_dir_all(d.join("build")).expect("mkdir");
        fs::write(d.join("build/junk.wrela"), "").expect("write");
        let files = find_files(&d).expect("ok");
        let mods: Vec<String> = files.iter().map(|f| f.module.join("::")).collect();
        assert_eq!(mods, ["main", "shapes::blob"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn bad_names_are_errors() {
        let d = tmp("names");
        fs::write(d.join("my-file.wrela"), "").expect("write");
        fs::write(d.join("fn.wrela"), "").expect("write");
        let errs = find_files(&d).expect_err("bad names");
        assert_eq!(errs.len(), 2);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn nested_packages_under_nonmodule_directories_are_not_modules() {
        let d = tmp("nested-package");
        fs::write(d.join("main.wrela"), "").expect("source");
        fs::create_dir_all(d.join("web-assets/vendor")).expect("directory");
        fs::write(d.join("web-assets/vendor/wrela.toml"), "").expect("manifest");
        fs::write(d.join("web-assets/vendor/main.wrela"), "").expect("package source");
        let files = find_files(&d).expect("nested package is not a module");
        assert_eq!(files.len(), 1);
        fs::write(d.join("web-assets/own.wrela"), "").expect("invalid module");
        assert!(find_files(&d).is_err(), "own modules still need valid directory names");
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directories_are_refused() {
        let d = tmp("links");
        let target = tmp("links-target");
        fs::write(target.join("x.wrela"), "").expect("write");
        std::os::unix::fs::symlink(&target, d.join("linked")).expect("symlink");
        let errs = find_files(&d).expect_err("refused");
        assert!(errs[0].message.contains("symbolic link to a directory"));
        assert!(errs[0].symlink, "reported as E0208");
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&target);
    }

    /// Links under a directory that isn't a module aren't followed: two links that make a
    /// cycle used to make the search for modules there run for hours.
    #[cfg(unix)]
    #[test]
    fn links_under_other_directories_are_not_followed() {
        let d = tmp("cycle");
        fs::write(d.join("main.wrela"), "").expect("write");
        fs::create_dir_all(d.join("web-assets/up")).expect("mkdir");
        for name in ["a", "b", "c"] {
            std::os::unix::fs::symlink("..", d.join("web-assets/up").join(name)).expect("link");
        }
        let start = std::time::Instant::now();
        let files = find_files(&d).expect("ok");
        assert_eq!(files.len(), 1);
        assert!(start.elapsed() < std::time::Duration::from_secs(5), "{:?}", start.elapsed());
        let _ = fs::remove_dir_all(&d);
    }

    /// A link to an output directory is skipped like the directory itself.
    #[cfg(unix)]
    #[test]
    fn linked_output_directories_are_skipped() {
        let d = tmp("outlink");
        let target = tmp("outlink-target");
        fs::write(d.join("main.wrela"), "").expect("write");
        for name in wrela_sema::OUTPUT_DIRS {
            std::os::unix::fs::symlink(&target, d.join(name)).expect("symlink");
        }
        assert_eq!(find_files(&d).expect("ok").len(), 1);
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&target);
    }
}
