//! Where a compile may read (language.md §15.5's `toolchain` service): anywhere the process can,
//! for the `wrela` commands, or only inside the roots a tool was granted. Every file a compile
//! reads goes through a [`Reach`]: the package's and its dependencies' sources and manifests,
//! the workspace's manifest and its members', the files `embed` reads, and a frame test's input.
//!
//! A confined read finds the file's canonical path, refuses one outside every root, and opens it
//! relative to that root's directory handle (`cap_std`), one part at a time. So a symbolic link
//! that leads out is refused, also one put in place of a directory after the check. A file is
//! opened without waiting (`O_NONBLOCK`), and the opened handle must be a regular file.

use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Where a compile may read: anywhere, or only inside its roots.
#[derive(Clone, Default)]
pub struct Reach {
    roots: Option<Arc<Vec<Root>>>,
}

impl std::fmt::Debug for Reach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.roots {
            None => f.write_str("Reach::anywhere"),
            Some(roots) => {
                f.debug_list().entries(roots.iter().map(|r| r.path.display().to_string())).finish()
            }
        }
    }
}

/// A root a confined compile reads in: its canonical path, and its directory, opened.
struct Root {
    path: PathBuf,
    dir: Dir,
}

/// What a path names, a link followed if `kind` was asked to follow it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Link,
    Other,
}

impl Kind {
    fn of(t: std::fs::FileType) -> Kind {
        if t.is_symlink() {
            Kind::Link
        } else if t.is_dir() {
            Kind::Dir
        } else if t.is_file() {
            Kind::File
        } else {
            Kind::Other
        }
    }

    fn of_cap(t: cap_std::fs::FileType) -> Kind {
        if t.is_symlink() {
            Kind::Link
        } else if t.is_dir() {
            Kind::Dir
        } else if t.is_file() {
            Kind::File
        } else {
            Kind::Other
        }
    }
}

/// A read refused because it leaves the roots.
fn outside(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!("`{}` is outside the roots the tool was granted", path.display()),
    )
}

impl Reach {
    /// Anywhere the process may read: the `wrela` commands'.
    pub fn anywhere() -> Reach {
        Reach::default()
    }

    /// Only inside `dirs`, each opened now: a directory that can't be opened is an error.
    pub fn within(dirs: &[PathBuf]) -> Result<Reach, String> {
        let mut roots = Vec::new();
        for d in dirs {
            let path = d.canonicalize().map_err(|e| format!("`{}`: {e}", d.display()))?;
            let dir = Dir::open_ambient_dir(&path, ambient_authority())
                .map_err(|e| format!("`{}`: {e}", d.display()))?;
            roots.push(Root { path, dir });
        }
        Ok(Reach { roots: Some(Arc::new(roots)) })
    }

    /// Only inside these directories, already opened: each one's real path and its handle (the
    /// tool host's grants, as it opened them).
    pub fn of_dirs(dirs: Vec<(PathBuf, Dir)>) -> Reach {
        let roots = dirs.into_iter().map(|(path, dir)| Root { path, dir }).collect();
        Reach { roots: Some(Arc::new(roots)) }
    }

    /// Whether only the roots may be read.
    pub fn is_confined(&self) -> bool {
        self.roots.is_some()
    }

    /// The root `canonical` is in, and its path inside it (empty for the root itself).
    fn locate<'a>(&'a self, roots: &'a [Root], canonical: &Path) -> Option<(&'a Root, PathBuf)> {
        roots.iter().find_map(|r| canonical.strip_prefix(&r.path).ok().map(|rel| (r, rel.into())))
    }

    /// `path`'s canonical path, if this reach may read it: an error for one outside the roots.
    pub fn canonical(&self, path: &Path) -> std::io::Result<PathBuf> {
        let canonical = path.canonicalize()?;
        match &self.roots {
            None => Ok(canonical),
            Some(roots) if self.locate(roots, &canonical).is_some() => Ok(canonical),
            Some(_) => Err(outside(path)),
        }
    }

    /// Whether this reach may read `path`, which may not be there yet: its nearest ancestor that
    /// is, canonical, is inside a root (always, unconfined).
    pub fn covers(&self, path: &Path) -> bool {
        let Some(roots) = &self.roots else { return true };
        let mut at = path.to_path_buf();
        loop {
            if let Ok(c) = at.canonicalize() {
                return self.locate(roots, &c).is_some();
            }
            if !at.pop() {
                return false;
            }
        }
    }

    /// The bytes of the regular file at `path`.
    pub fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        let Some(roots) = &self.roots else { return std::fs::read(path) };
        let canonical = path.canonicalize()?;
        let (root, rel) = self.locate(roots, &canonical).ok_or_else(|| outside(path))?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        cap_std::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
        let mut file = root.dir.open_with(&rel, &options)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::other("it isn't a regular file"));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// The text of the regular file at `path`, which must be UTF-8.
    pub fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
        if self.roots.is_none() {
            return std::fs::read_to_string(path);
        }
        String::from_utf8(self.read(path)?).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            )
        })
    }

    /// What's at `path`, following a final link if `follow`: `Ok(None)` if nothing is there. A
    /// link that leads out of the roots, followed, is an error.
    pub fn kind(&self, path: &Path, follow: bool) -> std::io::Result<Option<Kind>> {
        let missing = |e: std::io::Error| {
            if e.kind() == std::io::ErrorKind::NotFound { Ok(None) } else { Err(e) }
        };
        let Some(roots) = &self.roots else {
            let meta =
                if follow { std::fs::metadata(path) } else { std::fs::symlink_metadata(path) };
            return meta.map(|m| Some(Kind::of(m.file_type()))).or_else(missing);
        };
        // A root itself: its own handle says what it is, since its directory is outside it.
        if let Ok(c) = path.canonicalize()
            && let Some(root) = roots.iter().find(|r| r.path == c)
        {
            return root.dir.dir_metadata().map(|m| Some(Kind::of_cap(m.file_type())));
        }
        // The link itself is located by its directory, which must be in a root.
        let (parent, name) = match (path.parent(), path.file_name()) {
            (Some(p), Some(n)) if !p.as_os_str().is_empty() => (p, Some(n)),
            _ => (path, None),
        };
        let parent = match parent.canonicalize() {
            Ok(p) => p,
            Err(e) => return missing(e),
        };
        let (root, mut rel) = self.locate(roots, &parent).ok_or_else(|| outside(path))?;
        let Some(name) = name else {
            let meta = if rel.as_os_str().is_empty() {
                root.dir.dir_metadata()
            } else {
                root.dir.metadata(&rel)
            };
            return meta.map(|m| Some(Kind::of_cap(m.file_type()))).or_else(missing);
        };
        rel.push(name);
        let meta = if follow { root.dir.metadata(&rel) } else { root.dir.symlink_metadata(&rel) };
        match meta {
            Ok(m) => Ok(Some(Kind::of_cap(m.file_type()))),
            // cap-std's words for a path that resolves outside the directory.
            Err(e) if e.to_string().contains("outside of the filesystem") => Err(outside(path)),
            Err(e) => missing(e),
        }
    }

    /// Whether `path` is a regular file, a link to one followed.
    pub fn is_file(&self, path: &Path) -> bool {
        matches!(self.kind(path, true), Ok(Some(Kind::File)))
    }

    /// Whether `path` is a directory, a link to one followed.
    pub fn is_dir(&self, path: &Path) -> bool {
        matches!(self.kind(path, true), Ok(Some(Kind::Dir)))
    }

    /// The names of the entries of the directory at `path`, each with what it is (a link as a
    /// link), in no order.
    pub fn entries(&self, path: &Path) -> std::io::Result<Vec<(std::ffi::OsString, Kind)>> {
        let Some(roots) = &self.roots else {
            let mut out = Vec::new();
            for e in std::fs::read_dir(path)?.filter_map(Result::ok) {
                let Ok(t) = e.file_type() else { continue };
                out.push((e.file_name(), Kind::of(t)));
            }
            return Ok(out);
        };
        let canonical = path.canonicalize()?;
        let (root, rel) = self.locate(roots, &canonical).ok_or_else(|| outside(path))?;
        let entries = if rel.as_os_str().is_empty() {
            root.dir.entries()?
        } else {
            root.dir.read_dir(&rel)?
        };
        let mut out = Vec::new();
        for e in entries.filter_map(Result::ok) {
            let Ok(t) = e.file_type() else { continue };
            out.push((e.file_name(), Kind::of_cap(t)));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wrela-reach-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    #[cfg(unix)]
    #[test]
    fn a_confined_read_stays_in_its_roots_and_follows_no_link_out() {
        let base = scratch("confined");
        let (inside, outside_dir) = (base.join("in"), base.join("out"));
        std::fs::create_dir_all(inside.join("sub")).expect("in");
        std::fs::create_dir_all(&outside_dir).expect("out");
        std::fs::write(inside.join("sub/a.txt"), "a").expect("a");
        std::fs::write(outside_dir.join("secret.txt"), "s").expect("secret");
        std::os::unix::fs::symlink(outside_dir.join("secret.txt"), inside.join("link.txt"))
            .expect("a link out");
        std::os::unix::fs::symlink(&outside_dir, inside.join("dir-link")).expect("a dir link");
        let reach = Reach::within(std::slice::from_ref(&inside)).expect("opened");
        assert_eq!(reach.read(&inside.join("sub/a.txt")).expect("inside"), b"a");
        for bad in [inside.join("link.txt"), outside_dir.join("secret.txt")] {
            let e = reach.read(&bad).expect_err("outside");
            assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{bad:?}: {e}");
        }
        assert!(reach.entries(&outside_dir).is_err(), "a directory outside");
        assert!(reach.kind(&inside.join("dir-link"), true).is_err(), "a link followed out");
        assert_eq!(
            reach.kind(&inside.join("dir-link"), false).expect("the link"),
            Some(Kind::Link)
        );
        assert_eq!(reach.kind(&inside.join("none"), false).expect("missing"), None);
        assert!(reach.covers(&inside.join("not/yet")) && !reach.covers(&outside_dir));
        // A root itself is what its own handle says, though its directory is outside it.
        assert_eq!(reach.kind(&inside, false).expect("the root"), Some(Kind::Dir));
        assert!(reach.is_dir(&inside) && reach.entries(&inside).is_ok());
        let anywhere = Reach::anywhere();
        assert_eq!(anywhere.read(&inside.join("link.txt")).expect("anywhere"), b"s");
        let _ = std::fs::remove_dir_all(&base);
    }
}
