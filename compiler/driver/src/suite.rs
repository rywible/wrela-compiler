//! What `wrela suite` checks of a workspace beside its tests (language.md §13.2): which names it
//! takes, and the allowlist, `[workspace] allow`, over the workspace's files.
//!
//! **The workspace's files** are every file under its directory that its `.gitignore` files
//! don't ignore. Git's index isn't read, so the check needs no git and runs the same on a copy of
//! the files (an archive of them, say). A file that git would show as untracked is checked
//! too, so a stray file fails before it is committed. Only the versioned ignore files count: a
//! checkout's own excludes (`.git/info/exclude`, `core.excludesFile`) aren't read, so they hide
//! nothing and every checkout of the same files gives the same answer. The `.gitignore` files
//! are read as git reads them, apart from `[...]` classes, which match their text. `.git` is
//! never looked into, and a link is a file.
//!
//! **Each file is excluded, allowed, pending or refused.** One that an `exclude` glob matches
//! isn't looked at. Else one that an `allow` glob matches is allowed, and else one that a
//! `pending` glob matches is reported as waiting for removal. Any other file is refused, and so
//! is a `pending` glob that matches no file waiting: the list stays what it says.

use crate::manifest::WorkspaceDecl;
use crate::workspace::Workspace;
use std::path::Path;

/// Whether `s` is a glob of paths: parts separated by `/`, none of them empty, `.` or `..`, a
/// part with `**` being `**` alone.
pub fn is_glob(s: &str) -> bool {
    !s.is_empty()
        && s.split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && (p == "**" || !p.contains("**")))
}

/// Whether the path `path` (parts separated by `/`) matches `glob`: `*` matches any text within
/// a part, `?` one character, and a part `**` any number of parts.
pub fn matches(glob: &str, path: &str) -> bool {
    let glob: Vec<&str> = glob.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    parts_match(&glob, &path)
}

fn parts_match(glob: &[&str], path: &[&str]) -> bool {
    match glob.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|i| parts_match(rest, &path[i..])),
        Some((part, rest)) => path
            .split_first()
            .is_some_and(|(p, more)| part_matches(part, p) && parts_match(rest, more)),
    }
}

/// Whether one part of a path matches one part of a glob.
fn part_matches(glob: &str, part: &str) -> bool {
    let (g, p): (Vec<char>, Vec<char>) = (glob.chars().collect(), part.chars().collect());
    // The classic two-pointer match, going back to the last `*` on a mismatch.
    let (mut i, mut j, mut star, mut from) = (0, 0, None, 0);
    while j < p.len() {
        if i < g.len() && (g[i] == '?' || g[i] == p[j]) {
            i += 1;
            j += 1;
        } else if i < g.len() && g[i] == '*' {
            star = Some(i);
            from = j;
            i += 1;
        } else if let Some(s) = star {
            i = s + 1;
            from += 1;
            j = from;
        } else {
            return false;
        }
    }
    g[i..].iter().all(|&c| c == '*')
}

/// One line of an ignore file.
struct Rule {
    /// The directory of the file it's in, from the workspace's (empty at the top).
    base: String,
    glob: String,
    negated: bool,
    dirs_only: bool,
    /// It has a `/` before its end, so it matches paths from `base`, not names at any depth.
    anchored: bool,
}

impl Rule {
    /// The rules of an ignore file's `text`, in the directory `base`.
    fn read(base: &str, text: &str) -> Vec<Rule> {
        let mut rules = Vec::new();
        for line in text.lines() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (negated, line) = match line.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, line.strip_prefix('\\').unwrap_or(line)),
            };
            let (dirs_only, line) = match line.strip_suffix('/') {
                Some(rest) => (true, rest),
                None => (false, line),
            };
            let anchored = line.contains('/');
            let glob = line.trim_start_matches('/');
            if glob.is_empty() {
                continue;
            }
            // A trailing `/**` is what's inside the directory, at any depth, and not the
            // directory itself (a manifest's glob matches it too): one part, then any.
            let glob = match glob.strip_suffix("/**") {
                Some(dir) => format!("{dir}/*/**"),
                None => glob.to_string(),
            };
            rules.push(Rule { base: base.to_string(), glob, negated, dirs_only, anchored });
        }
        rules
    }

    /// Whether the rule matches the entry at `path` (from the workspace's directory).
    fn matches(&self, path: &str, dir: bool) -> bool {
        if self.dirs_only && !dir {
            return false;
        }
        let inside = if self.base.is_empty() {
            path
        } else {
            match path.strip_prefix(&self.base).and_then(|p| p.strip_prefix('/')) {
                Some(p) => p,
                None => return false,
            }
        };
        if self.anchored {
            matches(&self.glob, inside)
        } else {
            part_matches(&self.glob, inside.rsplit('/').next().unwrap_or(inside))
        }
    }
}

/// The workspace's files under `root` (see the module's comment): each path from `root`, with
/// `/` between its parts, sorted.
pub fn files(root: &Path) -> Result<Vec<String>, String> {
    let mut rules = Vec::new();
    let mut out = Vec::new();
    walk(root, "", &mut rules, &mut out)?;
    out.sort();
    Ok(out)
}

/// Adds the files in the directory `dir` (at `path` from the workspace's) to `out`, with the
/// rules of the ignore files above it, and its own.
fn walk(
    dir: &Path,
    path: &str,
    rules: &mut Vec<Rule>,
    out: &mut Vec<String>,
) -> Result<(), String> {
    let had = rules.len();
    match std::fs::read_to_string(dir.join(".gitignore")) {
        Ok(text) => rules.extend(Rule::read(path, &text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("can't read {}: {e}", dir.join(".gitignore").display())),
    }
    let unread = |e: std::io::Error| format!("can't read {}: {e}", dir.display());
    let mut entries: Vec<(String, bool)> = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(unread)? {
        let entry = entry.map_err(unread)?;
        // A link is a file, as git records it: what it leads to isn't looked into.
        let kind = entry.file_type().map_err(unread)?;
        let Some(name) = entry.file_name().to_str().map(String::from) else {
            return Err(format!("{}: a name that isn't UTF-8", entry.path().display()));
        };
        entries.push((name, kind.is_dir()));
    }
    entries.sort();
    for (name, is_dir) in entries {
        if name == ".git" {
            continue;
        }
        let at = if path.is_empty() { name.clone() } else { format!("{path}/{name}") };
        // The last rule that matches decides, as in git.
        let ignored =
            rules.iter().rev().find(|r| r.matches(&at, is_dir)).is_some_and(|r| !r.negated);
        if ignored {
            continue;
        }
        if is_dir {
            walk(&dir.join(&name), &at, rules, out)?;
        } else {
            out.push(at);
        }
    }
    rules.truncate(had);
    Ok(())
}

/// What the allowlist check found.
#[derive(Debug, Default, PartialEq)]
pub struct Allowed {
    /// How many files it looked at (none that `exclude` matches).
    pub checked: usize,
    /// The files no glob allows, and no `pending` glob matches.
    pub refused: Vec<String>,
    /// The files no glob allows that a `pending` glob matches.
    pub pending: Vec<String>,
    /// The `pending` globs that match no such file.
    pub stale: Vec<String>,
}

impl Allowed {
    pub fn passed(&self) -> bool {
        self.refused.is_empty() && self.stale.is_empty()
    }
}

/// The allowlist check of the files `files` (paths from the workspace's directory) against what
/// `decl` declares.
pub fn allowlist(decl: &WorkspaceDecl, files: &[String]) -> Allowed {
    let any =
        |globs: &[(String, wrela_diag::Span)], f: &str| globs.iter().any(|(g, _)| matches(g, f));
    let mut out = Allowed::default();
    for f in files {
        if any(&decl.exclude, f) {
            continue;
        }
        out.checked += 1;
        if any(&decl.allow, f) {
            continue;
        }
        if any(&decl.pending, f) {
            out.pending.push(f.clone());
        } else {
            out.refused.push(f.clone());
        }
    }
    out.stale = (decl.pending.iter())
        .filter(|(g, _)| !out.pending.iter().any(|f| matches(g, f)))
        .map(|(g, _)| g.clone())
        .collect();
    out
}

/// One of the names `wrela suite` takes.
#[derive(Clone, Debug, PartialEq)]
pub enum Selection {
    /// No name: every check.
    All,
    /// `fmt`: the members' `.wrela` files are formatted.
    Fmt,
    /// `allow`: the allowlist check.
    Allow,
    /// `wrela::<package-path>::`: every test of the member at that path.
    Group(String),
    /// `wrela::<package-path>::<test>`: exactly that test.
    Test(String, String),
}

/// The selection a command line's `names` make, as `tools/check.sh` reads its own: none, or
/// one name that is a check's, a member's group of tests or one test. An empty name, a second
/// one, a malformed one and a member the workspace hasn't are errors.
pub fn select(ws: &Workspace, names: &[String]) -> Result<Selection, String> {
    let name = match names {
        [] => return Ok(Selection::All),
        [name] if !name.trim().is_empty() => name.as_str(),
        _ => return Err("usage: wrela suite [check-name] (one name, not empty)".into()),
    };
    match name {
        "fmt" => return Ok(Selection::Fmt),
        "allow" => return Ok(Selection::Allow),
        _ => {}
    }
    let Some(rest) = name.strip_prefix("wrela::") else {
        return Err(format!(
            "unknown check `{name}`: the suite's checks are `fmt`, `allow`, `wrela::<package-path>::` and `wrela::<package-path>::<test-name>`"
        ));
    };
    let Some((package, test)) = rest.split_once("::") else {
        return Err(format!(
            "`{name}`: Wrela names are wrela::<package-path>::<test-name>, or wrela::<package-path>:: for every test in the package"
        ));
    };
    if test.contains("::") || (!test.is_empty() && !wrela_syntax::lexer::is_name(test)) {
        return Err(format!("`{name}`: `{test}` isn't a test's name"));
    }
    let paths: Vec<String> = ws.members.iter().map(|m| ws.path_of(&m.dir)).collect();
    if !paths.iter().any(|p| p == package) {
        return Err(format!(
            "`{name}`: the workspace has no member at `{package}` (its members: {})",
            paths.join(", ")
        ));
    }
    Ok(if test.is_empty() {
        Selection::Group(package.to_string())
    } else {
        Selection::Test(package.to_string(), test.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_parts() {
        let cases = [
            ("**/*.wrela", "engine/a.wrela", true),
            ("**/*.wrela", "a.wrela", true),
            ("**/*.wrela", "a.wrelax", false),
            ("*.wrela", "engine/a.wrela", false),
            ("**/LICENSE*", "ui/fonts/LICENSE.txt", true),
            ("**/OFL-*.txt", "ui/fonts/OFL-Inter.txt", true),
            ("docs/vision.md", "docs/vision.md", true),
            ("docs/vision.md", "docs/process.md", false),
            ("compiler/**", "compiler/std/a.wrela", true),
            ("compiler/**", "compilers/a", false),
            ("a/**/b", "a/b", true),
            ("a/**/b", "a/x/y/b", true),
            ("a?c", "abc", true),
            ("a?c", "ac", false),
            ("*", "x/y", false),
        ];
        for (glob, path, want) in cases {
            assert_eq!(matches(glob, path), want, "{glob} {path}");
        }
        for bad in ["", "/a", "a//b", "a/../b", "./a", "a**", "a/b**/c"] {
            assert!(!is_glob(bad), "{bad}");
        }
        assert!(is_glob("**/build/*.png") && is_glob("Cargo.toml"));
    }

    /// A fresh directory with `files` (path, text) in it.
    fn tree(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wrela-suite-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (path, text) in files {
            let p = d.join(path);
            std::fs::create_dir_all(p.parent().expect("a parent")).expect("mkdir");
            std::fs::write(p, text).expect("write");
        }
        d
    }

    #[test]
    fn ignore_files_leave_files_out_as_git_does() {
        let d = tree(
            "ignored",
            &[
                (
                    ".gitignore",
                    "# outputs\n/target/\n**/build/\n*.log\n!keep.log\nabc/**\n!abc/keep.py\n",
                ),
                ("target/x", ""),
                ("a/target/y", ""),
                ("a/build/z", ""),
                ("a/run.log", ""),
                ("a/keep.log", ""),
                ("a/b/.gitignore", "c.txt\n/d/\n"),
                ("a/b/c.txt", ""),
                ("a/b/d/e", ""),
                ("a/c.txt", ""),
                // A trailing `/**` matches what's inside `abc`, not `abc` itself, so a file in it
                // can be taken back (gitignore(5)).
                ("abc/keep.py", ""),
                ("abc/other.py", ""),
                ("abc/deeper/x.py", ""),
                (".git/HEAD", ""),
            ],
        );
        let found = files(&d).expect("walks");
        assert_eq!(
            found,
            [".gitignore", "a/b/.gitignore", "a/c.txt", "a/keep.log", "a/target/y", "abc/keep.py"]
        );
        let decl = crate::manifest::parse(
            wrela_diag::FileId(0),
            "[workspace]\nmembers = []\nallow = [\"**/.gitignore\", \"**/*.log\", \"**/*.txt\", \"a/target/*\"]\n",
        )
        .expect("reads")
        .workspace
        .expect("a workspace");
        assert_eq!(allowlist(&decl, &found).refused, ["abc/keep.py"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Only the versioned ignore files count: a checkout's own `.git/info/exclude` can't hide a
    /// file, so the same files give the same answer in every checkout and in a copy.
    #[test]
    fn a_checkouts_own_excludes_hide_nothing() {
        let files_of = |name: &str, exclude: bool| {
            let mut fs = vec![("secret.py", ""), (".git/HEAD", "")];
            if exclude {
                fs.push((".git/info/exclude", "secret.py\n"));
            }
            let d = tree(name, &fs);
            let found = files(&d).expect("walks");
            let _ = std::fs::remove_dir_all(&d);
            found
        };
        assert_eq!(files_of("excluded", true), files_of("plain", false));
        assert_eq!(files_of("excluded", true), ["secret.py"]);
    }

    /// The names the suite takes, as `tools/check.sh` reads its own: none, or one that is a
    /// check's, a member's group or a member's test; anything else is refused.
    #[test]
    fn a_selection_names_one_check_group_or_test() {
        let d = tree(
            "select",
            &[
                ("wrela.toml", "[workspace]\nmembers = [\"lib\", \"games/*\"]\n"),
                ("lib/wrela.toml", ""),
                ("games/floor/wrela.toml", ""),
            ],
        );
        let ws = Workspace::open(&d.join("wrela.toml")).expect("opens");
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(select(&ws, &[]), Ok(Selection::All));
        assert_eq!(select(&ws, &names(&["fmt"])), Ok(Selection::Fmt));
        assert_eq!(select(&ws, &names(&["allow"])), Ok(Selection::Allow));
        assert_eq!(select(&ws, &names(&["wrela::lib::"])), Ok(Selection::Group("lib".into())));
        assert_eq!(
            select(&ws, &names(&["wrela::games/floor::walks"])),
            Ok(Selection::Test("games/floor".into(), "walks".into()))
        );
        for (bad, says) in [
            (&[""][..], "usage"),
            (&[" "], "usage"),
            (&["fmt", "allow"], "usage"),
            (&["rustfmt"], "unknown check"),
            (&["wrela::lib"], "Wrela names are"),
            (&["wrela::lib::a::b"], "isn't a test's name"),
            (&["wrela::lib::not-a-name"], "isn't a test's name"),
            (&["wrela::games::"], "no member at `games`"),
            (&["wrela::::"], "no member at ``"),
            (&["wrela::./lib::"], "no member at `./lib`"),
        ] {
            let got = select(&ws, &names(bad));
            assert!(got.as_ref().is_err_and(|e| e.contains(says)), "{bad:?}: {got:?}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_allowlist_refuses_names_and_reports_what_waits() {
        let decl = crate::manifest::parse(
            wrela_diag::FileId(0),
            "[workspace]\nmembers = []\nallow = [\"**/*.wrela\", \"**/wrela.toml\"]\nexclude = [\"compiler/**\"]\npending = [\"games/console.toml\", \"gone.txt\"]\n",
        )
        .expect("reads")
        .workspace
        .expect("a workspace");
        let files: Vec<String> =
            ["a/main.wrela", "a/wrela.toml", "compiler/x.rs", "games/console.toml", "tools/x.py"]
                .map(String::from)
                .to_vec();
        let found = allowlist(&decl, &files);
        assert_eq!(found.checked, 4);
        assert_eq!(found.refused, ["tools/x.py"]);
        assert_eq!(found.pending, ["games/console.toml"]);
        assert_eq!(found.stale, ["gone.txt"]);
        assert!(!found.passed());
    }
}
