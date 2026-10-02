//! Finding a package's files (Q1 of #6): one package, a file is a module, a directory is a
//! module tree. `shapes/blob.wrela` is the module `shapes::blob`; `main.wrela` is the entry.
//!
//! Symbolic links to directories are refused, loudly (the open question on #6): following them
//! needs canonical-path dedup and cycle protection, and until that's decided a check must never
//! pass without reading the files behind a link.

use std::fs;
use std::path::{Path, PathBuf};

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

/// Directories that never hold modules: build output and run results.
const SKIPPED_DIRS: &[&str] = &["build", "results", "node_modules", "target"];

pub fn find_files(root: &Path) -> Result<Vec<PackageFile>, Vec<LayoutError>> {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    walk(root, root, &mut Vec::new(), &mut files, &mut errors);
    files.sort_by(|a, b| a.module.cmp(&b.module));
    if errors.is_empty() { Ok(files) } else { Err(errors) }
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && s != "_"
        && wrela_syntax::TokenKind::keyword(s).is_none()
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
            if prefix.is_empty() && SKIPPED_DIRS.contains(&name.as_str()) {
                continue;
            }
            if !is_ident(&name) {
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
        if !is_ident(stem) {
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

fn has_wrela_files(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|e| {
            let p = e.path();
            p.extension().is_some_and(|x| x == "wrela") || (p.is_dir() && has_wrela_files(&p))
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
}
