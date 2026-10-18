//! Workspaces (language.md §3.6): a `wrela.toml` with `[workspace]` names its member packages,
//! and a package at or below its directory depends on a member by name, `name = { workspace =
//! true }`. A package's workspace is the nearest manifest with `[workspace]` at or above its
//! directory. A member's name is its `[package] name`, else its directory's with `-` read as `_`.
//!
//! The loader ([`crate::check`] and the rest) reports what's wrong as diagnostics: E0220 for a
//! dependency no workspace has, E0223 for a workspace whose members aren't packages or share a
//! name. Tools that only need the directories use [`dependency_dirs`].

use crate::manifest::{self, Source, WorkspaceDecl};
use crate::reach::{Kind, Reach};
use std::path::{Path, PathBuf};
use wrela_diag::{Diagnostic, FileId, Span, codes};

/// A workspace's members.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Members {
    pub list: Vec<Member>,
    /// A member's manifest didn't read, so its name isn't known: what's wrong with it is
    /// reported where it was read, and a dependency no member has needn't be reported too.
    pub unread: bool,
}

/// One member of a workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub name: String,
    /// Its directory, canonical.
    pub dir: PathBuf,
}

/// The nearest manifest at or above `dir` that declares a workspace: its path.
pub fn find(dir: &Path) -> Option<PathBuf> {
    find_in(&Reach::anywhere(), dir)
}

/// [`find`], looking only where `reach` may read: a confined search stops at the top of the
/// roots, and never reads a manifest above them.
pub fn find_in(reach: &Reach, dir: &Path) -> Option<PathBuf> {
    let mut at = reach.canonical(dir).ok()?;
    loop {
        if !reach.covers(&at) {
            return None;
        }
        let path = at.join("wrela.toml");
        // A regular file only: a pipe named `wrela.toml` would block.
        if reach.is_file(&path)
            && reach.read_to_string(&path).is_ok_and(|t| manifest::declares_workspace(&t))
        {
            return Some(path);
        }
        if !at.pop() {
            return None;
        }
    }
}

/// The members of the workspace `decl` declares, in the manifest at `path`, each `/*` read as
/// the directories in it that have a `wrela.toml`, and what's wrong with them (E0223). A
/// directory listed twice is one member. `manifest_of` reads a member's manifest, by its path
/// in its canonical directory: `None` when it doesn't read (and the caller reports why).
pub fn members(
    path: &Path,
    decl: &WorkspaceDecl,
    manifest_of: &mut dyn FnMut(&Path) -> Option<manifest::Manifest>,
) -> (Members, Vec<Diagnostic>) {
    members_in(&Reach::anywhere(), path, decl, manifest_of)
}

/// [`members`], reading only what `reach` may: a member outside it is an error (E0223).
pub fn members_in(
    reach: &Reach,
    path: &Path,
    decl: &WorkspaceDecl,
    manifest_of: &mut dyn FnMut(&Path) -> Option<manifest::Manifest>,
) -> (Members, Vec<Diagnostic>) {
    let base = path.parent().unwrap_or(Path::new("."));
    let mut out: Vec<(Member, Span)> = Vec::new();
    let mut unread = false;
    let mut errors = Vec::new();
    let bad = |span: Span, msg: String| Diagnostic::new(codes::E0223, span, msg);
    for (entry, span) in &decl.members {
        let span = *span;
        let dirs = if let Some(parent) = entry.strip_suffix("/*").or((entry == "*").then_some("")) {
            let parent_dir = base.join(parent);
            let Ok(entries) = reach.entries(&parent_dir) else {
                errors.push(
                    bad(
                        span,
                        format!(
                            "`{entry}` names the packages in `{parent}`, which isn't a directory"
                        ),
                    )
                    .with_help("fix the path, which is relative to this `wrela.toml`"),
                );
                continue;
            };
            let mut dirs: Vec<PathBuf> = entries
                .into_iter()
                .filter(|(name, _)| !name.to_string_lossy().starts_with('.'))
                .filter(|(_, kind)| matches!(kind, Kind::Dir | Kind::Link))
                .map(|(name, _)| parent_dir.join(name))
                .filter(|p| reach.is_dir(p) && reach.is_file(&p.join("wrela.toml")))
                .collect();
            dirs.sort();
            if dirs.is_empty() {
                errors.push(bad(
                    span,
                    format!(
                        "`{entry}` names no package: no directory in `{parent}` has a `wrela.toml`"
                    ),
                ));
            }
            dirs
        } else {
            let dir = base.join(entry);
            if !reach.covers(&dir) {
                errors.push(bad(
                    span,
                    format!("the member `{entry}` is outside the roots the tool was granted"),
                ));
                continue;
            }
            if !reach.is_dir(&dir) {
                errors.push(
                    bad(span, format!("the member `{entry}` isn't a directory"))
                        .with_help("fix the path, which is relative to this `wrela.toml`"),
                );
                continue;
            }
            if !reach.is_file(&dir.join("wrela.toml")) {
                errors.push(
                    bad(span, format!("the member `{entry}` has no `wrela.toml`, so it isn't a package"))
                        .with_help(format!("give it one, with `[package]` and `name = \"...\"`, or take `{entry}` out of `members`")),
                );
                continue;
            }
            vec![dir]
        };
        for dir in dirs {
            let Ok(canon) = reach.canonical(&dir) else { continue };
            if out.iter().any(|(m, _)| m.dir == canon) {
                continue;
            }
            let Some(m) = manifest_of(&canon.join("wrela.toml")) else {
                unread = true;
                continue;
            };
            let Some(name) = member_name(&m, &canon) else {
                errors.push(
                    bad(span, format!("the member `{}` has no name", dir.display()))
                        .with_help("give its `wrela.toml` a `[package]` with `name = \"...\"`"),
                );
                continue;
            };
            if let Some((other, _)) = out.iter().find(|(m, _)| m.name == name) {
                errors.push(
                    bad(
                        span,
                        format!(
                            "two members are named `{name}`: `{}` and `{}`",
                            relative(&other.dir, base),
                            relative(&canon, base)
                        ),
                    )
                    .with_note(
                        "a dependency names a member by its name, so each member's must be its own",
                    )
                    .with_help("rename one, in its `[package] name`"),
                );
                continue;
            }
            out.push((Member { name, dir: canon }, span));
        }
    }
    (Members { list: out.into_iter().map(|(m, _)| m).collect(), unread }, errors)
}

/// The name of the member in `dir` whose manifest is `m`: its `[package] name`, else its
/// directory's with `-` read as `_`.
fn member_name(m: &manifest::Manifest, dir: &Path) -> Option<String> {
    m.name.as_ref().map(|(n, _)| n.clone()).or_else(|| {
        let n = dir.file_name()?.to_string_lossy().replace('-', "_");
        wrela_syntax::lexer::is_name(&n).then_some(n)
    })
}

/// The manifest at `path`, or why it doesn't read.
fn read(path: &Path) -> Result<manifest::Manifest, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    manifest::parse(FileId(0), &text)
        .map_err(|e| format!("{} doesn't read: {}", path.display(), e[0].message))
}

/// `path` relative to `base`, with `/` between its parts: how a message names a member.
fn relative(path: &Path, base: &Path) -> String {
    crate::lift::relative(path, base)
}

/// The directory of each dependency the manifest of the package at `pkg` declares, by name,
/// workspace dependencies resolved: for tools that read a package's dependencies without
/// loading them. The first problem is the error.
pub fn dependency_dirs(pkg: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let path = pkg.join("wrela.toml");
    let m = read(&path)?;
    let mut members: Option<Vec<Member>> = None;
    let mut out = Vec::new();
    for d in m.deps {
        let dir = match d.source {
            Source::Path(p) => pkg.join(p),
            Source::Workspace => {
                if members.is_none() {
                    members = Some(workspace_members(pkg)?);
                }
                let found = members.iter().flatten().find(|m| m.name == d.name);
                found
                    .ok_or_else(|| {
                        format!(
                            "{}: the workspace has no member named `{}`",
                            path.display(),
                            d.name
                        )
                    })?
                    .dir
                    .clone()
            }
        };
        out.push((d.name, dir));
    }
    Ok(out)
}

/// The members of the workspace of the package at `pkg`.
fn workspace_members(pkg: &Path) -> Result<Vec<Member>, String> {
    let path = find(pkg).ok_or_else(|| {
        format!(
            "{} has a workspace dependency, but no `wrela.toml` at or above it has `[workspace]`",
            pkg.display()
        )
    })?;
    let decl = read(&path)?.workspace.expect("found by its `[workspace]`");
    let mut unread = None;
    let (members, errors) = members(&path, &decl, &mut |p| {
        read(p).map_err(|e| unread = unread.take().or(Some(e))).ok()
    });
    if let Some(e) = unread {
        return Err(e);
    }
    match errors.first() {
        Some(e) => Err(format!("{}: {}", path.display(), e.message)),
        None => Ok(members.list),
    }
}

/// The manifest `text` with each `{ workspace = true }` dependency written as a `path`, the one
/// `path_of` gives its name (`None` is an error): for a copy of a package outside its workspace.
/// The rest of the text is kept as it is.
pub fn with_paths(text: &str, path_of: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let m = manifest::parse(FileId(0), text).map_err(|e| e[0].message.clone())?;
    let mut out = String::new();
    let mut from = 0;
    for d in m.deps.iter().filter(|d| d.source == Source::Workspace) {
        let path =
            path_of(&d.name).ok_or_else(|| format!("no path for the dependency `{}`", d.name))?;
        out.push_str(&text[from..d.span.start as usize]);
        out.push_str(&format!("{} = {{ path = {path:?} }}", d.name));
        from = d.span.end as usize;
    }
    out.push_str(&text[from..]);
    Ok(out)
}

/// The manifest `text` of the package at `pkg`, each workspace dependency written as the
/// absolute path of the member it is: for a copy of the package elsewhere.
pub fn detached(pkg: &Path, text: &str) -> Result<String, String> {
    let dirs = dependency_dirs(pkg)?;
    with_paths(text, |name| {
        dirs.iter().find(|(n, _)| n == name).map(|(_, d)| d.display().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory with `files` (path, text) in it.
    fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wrela-workspace-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (path, text) in files {
            let p = d.join(path);
            std::fs::create_dir_all(p.parent().expect("a parent")).expect("mkdir");
            std::fs::write(p, text).expect("write");
        }
        d
    }

    const FRAME: &str = "pub fn frame(time: f32, width: u32, height: u32) {}\n";

    /// The errors of checking the package at `dir`: each one's code and message.
    fn errors(dir: &Path) -> Vec<(&'static str, String)> {
        crate::check(dir)
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| (d.code.as_str(), d.message.clone()))
            .collect()
    }

    #[test]
    fn members_are_found_by_name_and_by_globs() {
        let d = tree(
            "found",
            &[
                (
                    "wrela.toml",
                    "[workspace]\nmembers = [\n    \"libs/*\",  # each library\n    \"app\",\n]\n",
                ),
                ("libs/geo-shapes/wrela.toml", ""),
                ("libs/geo-shapes/area.wrela", "pub fn unit() -> f32 {\n    1.0\n}\n"),
                (
                    "libs/solid/wrela.toml",
                    "[package]\nname = \"solid\"\n\n[dependencies]\ngeo_shapes = { workspace = true }\n",
                ),
                (
                    "libs/solid/volume.wrela",
                    "pub fn unit() -> f32 {\n    geo_shapes::area::unit()\n}\n",
                ),
                ("libs/.hidden/wrela.toml", "[package]\nname = \"solid\"\n"),
                ("libs/notes/readme.txt", "not a package"),
                (
                    "app/wrela.toml",
                    "[package]\nname = \"app\"\n\n[dependencies]\nsolid = { workspace = true }\ngeo_shapes = { workspace = true }\n",
                ),
                (
                    "app/main.wrela",
                    &format!(
                        "fn size() -> f32 {{\n    solid::volume::unit() + geo_shapes::area::unit()\n}}\n\n{FRAME}"
                    ),
                ),
            ],
        );
        assert_eq!(errors(&d.join("app")), []);
        let dirs = dependency_dirs(&d.join("app")).expect("resolves");
        let libs = d.join("libs").canonicalize().expect("libs");
        assert_eq!(
            dirs,
            [
                ("solid".to_string(), libs.join("solid")),
                ("geo_shapes".to_string(), libs.join("geo-shapes"))
            ]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_nearest_workspace_wins() {
        let d = tree(
            "nearest",
            &[
                ("wrela.toml", "[workspace]\nmembers = [\"outer\"]\n"),
                ("outer/wrela.toml", "[package]\nname = \"lib\"\n"),
                ("outer/far.wrela", "pub fn far() -> u32 {\n    1\n}\n"),
                // The inner workspace is a package too, and a member of itself.
                (
                    "inner/wrela.toml",
                    "[package]\nname = \"app\"\n\n[workspace]\nmembers = [\".\", \"lib\"]\n\n[dependencies]\nlib = { workspace = true }\n",
                ),
                (
                    "inner/main.wrela",
                    &format!("fn n() -> u32 {{\n    lib::near::near()\n}}\n\n{FRAME}"),
                ),
                ("inner/lib/wrela.toml", "[package]\nname = \"lib\"\n"),
                ("inner/lib/near.wrela", "pub fn near() -> u32 {\n    2\n}\n"),
            ],
        );
        assert_eq!(errors(&d.join("inner")), []);
        let found = find(&d.join("inner/lib")).expect("a workspace");
        assert_eq!(found, d.join("inner/wrela.toml").canonicalize().expect("canonical"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn what_a_workspace_gets_wrong_is_reported() {
        let app = |deps: &str| format!("[package]\nname = \"app\"\n\n[dependencies]\n{deps}\n");
        // Each case: its name, its files, and the error it gives: its code and what it says.
        type Case = (&'static str, Vec<(&'static str, String)>, &'static str, &'static str);
        let cases: [Case; 7] = [
            (
                "none",
                vec![("app/wrela.toml", app("lib = { workspace = true }"))],
                codes::E0220.as_str(),
                "no `wrela.toml` at or above",
            ),
            (
                "unnamed",
                vec![
                    ("wrela.toml", "[workspace]\nmembers = [\"app\", \"lib\"]\n".into()),
                    ("lib/wrela.toml", "[package]\nname = \"other\"\n".into()),
                    ("app/wrela.toml", app("lib = { workspace = true }")),
                ],
                codes::E0220.as_str(),
                "has no member named `lib`",
            ),
            (
                "twice",
                vec![
                    ("wrela.toml", "[workspace]\nmembers = [\"app\", \"a\", \"b\"]\n".into()),
                    ("a/wrela.toml", "[package]\nname = \"lib\"\n".into()),
                    ("b/wrela.toml", "[package]\nname = \"lib\"\n".into()),
                    ("app/wrela.toml", app("lib = { workspace = true }")),
                ],
                codes::E0223.as_str(),
                "two members are named `lib`: `a` and `b`",
            ),
            (
                "missing",
                vec![
                    ("wrela.toml", "[workspace]\nmembers = [\"app\", \"gone\"]\n".into()),
                    ("app/wrela.toml", app("app2 = { workspace = true }")),
                ],
                codes::E0223.as_str(),
                "the member `gone` isn't a directory",
            ),
            (
                "no-manifest",
                vec![
                    ("wrela.toml", "[workspace]\nmembers = [\"app\", \"lib\"]\n".into()),
                    ("lib/lib.wrela", "".into()),
                    ("app/wrela.toml", app("lib = { workspace = true }")),
                ],
                codes::E0223.as_str(),
                "the member `lib` has no `wrela.toml`",
            ),
            (
                "empty-glob",
                vec![
                    ("wrela.toml", "[workspace]\nmembers = [\"app\", \"libs/*\"]\n".into()),
                    ("libs/notes.txt", "".into()),
                    ("app/wrela.toml", app("lib = { workspace = true }")),
                ],
                codes::E0223.as_str(),
                "`libs/*` names no package",
            ),
            (
                "cycle",
                vec![
                    ("wrela.toml", "[workspace]\nmembers = [\"*\"]\n".into()),
                    (
                        "lib/wrela.toml",
                        "[package]\nname = \"lib\"\n\n[dependencies]\napp = { workspace = true }\n"
                            .into(),
                    ),
                    ("lib/lib.wrela", "".into()),
                    ("app/wrela.toml", app("lib = { workspace = true }")),
                ],
                codes::E0221.as_str(),
                "packages depend on each other",
            ),
        ];
        for (name, files, code, says) in cases {
            let mut files: Vec<(&str, &str)> =
                files.iter().map(|(p, t)| (*p, t.as_str())).collect();
            files.push(("app/main.wrela", FRAME));
            let d = tree(name, &files);
            let got = errors(&d.join("app"));
            assert!(
                got.iter().any(|(c, m)| *c == code && m.contains(says)),
                "{name}: expected {code} `{says}`, got {got:?}"
            );
            if code != codes::E0221.as_str() {
                assert!(
                    dependency_dirs(&d.join("app")).is_err(),
                    "{name}: tools see the error too"
                );
            }
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// A package's own `[workspace]` is checked though no dependency is found through it.
    #[test]
    fn a_packages_own_workspace_is_checked() {
        let d = tree(
            "own",
            &[
                ("wrela.toml", "[package]\nname = \"app\"\n\n[workspace]\nmembers = [\"gone\"]\n"),
                ("main.wrela", FRAME),
            ],
        );
        let got = errors(&d);
        assert!(
            got.iter()
                .any(|(c, m)| *c == "E0223" && m.contains("the member `gone` isn't a directory")),
            "{got:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A member whose manifest doesn't read is reported as that manifest's error, where it is,
    /// and not as a dependency no member has.
    #[test]
    fn a_members_manifest_that_doesnt_read_is_its_error() {
        let d = tree(
            "unread",
            &[
                ("wrela.toml", "[workspace]\nmembers = [\"app\", \"geo\"]\n"),
                ("geo/wrela.toml", "[package]\nname = 3\n"),
                (
                    "app/wrela.toml",
                    "[package]\nname = \"app\"\n\n[dependencies]\ngeo = { workspace = true }\n",
                ),
                ("app/main.wrela", FRAME),
            ],
        );
        let out = crate::check(&d.join("app"));
        let got: Vec<(&str, String)> = (out.diagnostics.iter().filter(|d| d.is_error()))
            .map(|d| {
                let file = d.span().map(|s| out.sources.file(s.file).name.clone());
                (d.code.as_str(), file.unwrap_or_default())
            })
            .collect();
        assert_eq!(got, [("E0219", "../geo/wrela.toml".to_string())]);
        assert!(dependency_dirs(&d.join("app")).is_err_and(|e| e.contains("geo")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_workspace_alone_is_not_a_package() {
        let d =
            tree("alone", &[("wrela.toml", "[workspace]\nmembers = []\n"), ("main.wrela", FRAME)]);
        assert_eq!(errors(&d).iter().map(|e| e.0).collect::<Vec<_>>(), [codes::E0223.as_str()]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn copies_name_their_dependencies_by_path() {
        let d = tree(
            "detached",
            &[
                ("wrela.toml", "[workspace]\nmembers = [\"*\"]\n"),
                ("lib/wrela.toml", "[package]\nname = \"lib\"\n"),
                (
                    "app/wrela.toml",
                    "# the app\n[package]\nname = \"app\"\n\n[dependencies]\nlib = { workspace = true }  # by name\nother = { path = \"../other\" }\n",
                ),
            ],
        );
        let text = std::fs::read_to_string(d.join("app/wrela.toml")).expect("read");
        let lib = d.join("lib").canonicalize().expect("lib").display().to_string();
        assert_eq!(
            detached(&d.join("app"), &text).expect("detached"),
            format!(
                "# the app\n[package]\nname = \"app\"\n\n[dependencies]\nlib = {{ path = {lib:?} }}  # by name\nother = {{ path = \"../other\" }}\n"
            )
        );
        assert!(with_paths(&text, |_| None).is_err(), "a workspace dependency with no path");
        let _ = std::fs::remove_dir_all(&d);
    }
}
