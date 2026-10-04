//! Finding a package's files (Q1 of #6): one package, a file is a module, a directory is a
//! module tree. `shapes/blob.wrela` is the module `shapes::blob`; `main.wrela` is the entry.
//!
//! Symbolic links to directories are refused, loudly (the open question on #6): following them
//! needs canonical-path dedup and cycle protection, and until that's decided a check must never
//! pass without reading the files behind a link.

use std::fs;
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
    let mut files = Vec::new();
    let mut errors = Vec::new();
    walk(root, root, &mut Vec::new(), &mut files, &mut errors);
    files.sort_by(|a, b| a.module.cmp(&b.module));
    if errors.is_empty() { Ok(files) } else { Err(errors) }
}

fn walk(
    root: &Path,
    dir: &Path,
    prefix: &mut Vec<String>,
    files: &mut Vec<PackageFile>,
    errors: &mut Vec<LayoutError>,
) {
    let display = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
    let entries = match fs::read_dir(dir) {
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
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        // Skipped whatever they are: a link to an output directory elsewhere is no module.
        if prefix.is_empty() && wrela_sema::OUTPUT_DIRS.contains(&name.as_str()) {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if meta.file_type().is_symlink() {
            if fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
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
        } else if meta.is_dir() {
            // A directory with a manifest is another package, not part of this one's tree.
            if path.join("wrela.toml").is_file() {
                continue;
            }
            if !is_name(&name) {
                if has_wrela_files(&path) {
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
            walk(root, &path, prefix, files, errors);
            prefix.pop();
            continue;
        }
        let Some(stem) = name.strip_suffix(".wrela") else { continue };
        // A pipe or a device would block or never end when read.
        let regular = if meta.file_type().is_symlink() {
            fs::metadata(&path).is_ok_and(|m| m.is_file())
        } else {
            meta.is_file()
        };
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
/// entries are skipped and symbolic links aren't followed (so a cycle of links ends).
fn has_wrela_files(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|e| {
            let p = e.path();
            if e.file_name().to_string_lossy().starts_with('.') {
                return false;
            }
            p.extension().is_some_and(|x| x == "wrela")
                || (e.file_type().is_ok_and(|t| t.is_dir()) && has_wrela_files(&p))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
