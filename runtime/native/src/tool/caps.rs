//! What a tool may reach (language.md §15.5): the grants of its command line, opened once into
//! one capability context ([`Caps`]) that every service is given. A root is a directory opened
//! as a handle (`cap_std::fs::Dir`), and every path is opened relative to it, one part at a time,
//! so no path, `..` or symbolic link leaves it, even a link put in place of a directory after a
//! check.
//!
//! A file is opened without waiting (`O_NONBLOCK`), so a FIFO with nothing at its other end can't
//! hold a worker, and the opened handle must be a regular file: a FIFO, a socket or a device is
//! refused, by the handle's own type, not the path's.

use cap_std::ambient_authority;
use cap_std::fs::{Dir, File, FileType, OpenOptions};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

/// The most bytes a whole file read or written at once holds (`std::tool::MOST_BYTES`): a larger
/// one goes through a blob.
pub const MOST_BYTES: usize = 512 * 1024;

/// What the command line grants a tool, before anything is opened: roots by name (`--read
/// NAME=DIR`, `--write NAME=DIR`) and secrets by name (`--secret NAME`).
#[derive(Clone, Debug, Default)]
pub struct Grants {
    /// Each root: its name, its directory, and whether the tool may change it.
    pub roots: Vec<(String, PathBuf, bool)>,
    /// The environment variables the tool may read, by name.
    pub secrets: Vec<String>,
}

impl Grants {
    /// Grants the directory `dir` as the root `name`, for reading, or for writing (and reading)
    /// if `write`.
    pub fn root(mut self, name: &str, dir: impl Into<PathBuf>, write: bool) -> Grants {
        self.roots.push((name.to_string(), dir.into(), write));
        self
    }

    /// Grants the secret `name`.
    pub fn secret(mut self, name: &str) -> Grants {
        self.secrets.push(name.to_string());
        self
    }

    /// Reads one grant of a command line: `--read NAME=DIR`, `--write NAME=DIR` or `--secret
    /// NAME`, as the flag and its value. `Ok(false)` if `flag` isn't a grant's.
    pub fn parse(&mut self, flag: &str, value: &str) -> Result<bool, String> {
        let write = match flag {
            "--read" => false,
            "--write" => true,
            "--secret" => {
                self.secrets.push(value.to_string());
                return Ok(true);
            }
            _ => return Ok(false),
        };
        let Some((name, dir)) = value.split_once('=') else {
            return Err(format!("`{flag} {value}`: a root is granted as `{flag} NAME=DIR`"));
        };
        self.roots.push((name.to_string(), PathBuf::from(dir), write));
        Ok(true)
    }
}

/// A root, opened.
struct Root {
    dir: Dir,
    write: bool,
}

/// What a path names, as `std::tool::files::Kind` numbers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File = 0,
    Dir = 1,
    Link = 2,
    Other = 3,
}

impl Kind {
    fn of(t: FileType) -> Kind {
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

/// A directory's entry, or what's at a path: its name (empty for a path's), kind and size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    pub size: u64,
}

/// The capability context: the roots, opened, and the secrets, read from the environment once,
/// when the tool starts. Every service reaches files and secrets through it alone.
pub struct Caps {
    roots: BTreeMap<String, Root>,
    secrets: Vec<(String, String)>,
}

impl std::fmt::Debug for Caps {
    /// Names the roots and the secrets, never the secrets' values.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let roots: Vec<(&String, bool)> = self.roots.iter().map(|(n, r)| (n, r.write)).collect();
        let secrets: Vec<&String> = self.secrets.iter().map(|(n, _)| n).collect();
        f.debug_struct("Caps").field("roots", &roots).field("secrets", &secrets).finish()
    }
}

/// Whether `name` can name a root or a secret: letters, digits, `_`, `-` and `.`, but not `.`
/// or `..` alone.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
}

/// `e`, an error of `path`, in a tool's words: a path that a link led out of its root says so.
fn failed(path: &str, e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        return format!("`{path}` isn't there");
    }
    // cap-std's words for a path that resolves outside the directory it's opened from.
    if e.to_string().contains("outside of the filesystem") {
        return format!("`{path}` leads outside its root");
    }
    format!("`{path}`: {e}")
}

/// Opens the file `rel` of `dir` (`path`, as the tool names it) for reading, or for writing
/// (made if it isn't there, then emptied), without waiting on it: a regular file, or an error.
fn open_file(dir: &Dir, rel: &str, path: &str, write: bool) -> Result<File, String> {
    let mut options = OpenOptions::new();
    if write {
        options.write(true).create(true);
    } else {
        options.read(true);
    }
    #[cfg(unix)]
    cap_std::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
    let file = dir.open_with(rel, &options).map_err(|e| failed(path, e))?;
    let kind = file.metadata().map_err(|e| failed(path, e))?.file_type();
    if !kind.is_file() {
        return Err(format!("`{path}` isn't a regular file"));
    }
    // Emptied only once it's known to be a file.
    if write {
        file.set_len(0).map_err(|e| failed(path, e))?;
    }
    Ok(file)
}

impl Caps {
    /// Opens `grants`: each root's directory, and each secret from `env` (the environment, for
    /// `wrela tool`). A root that isn't a directory, a name given twice and a secret the
    /// environment doesn't have are errors, before the tool starts.
    pub fn open(grants: &Grants, env: impl Fn(&str) -> Option<String>) -> Result<Caps, String> {
        let mut roots = BTreeMap::new();
        for (name, dir, write) in &grants.roots {
            if !valid_name(name) {
                return Err(format!(
                    "`{name}` can't name a root: use letters, digits, `_`, `-` and `.`"
                ));
            }
            if roots.contains_key(name) {
                return Err(format!("the root `{name}` is granted twice"));
            }
            let opened = Dir::open_ambient_dir(dir, ambient_authority())
                .map_err(|e| format!("the root `{name}` ({}): {e}", dir.display()))?;
            roots.insert(name.clone(), Root { dir: opened, write: *write });
        }
        let mut secrets: Vec<(String, String)> = Vec::new();
        for name in &grants.secrets {
            if !valid_name(name) {
                return Err(format!("`{name}` can't name a secret"));
            }
            if secrets.iter().any(|(n, _)| n == name) {
                continue;
            }
            let Some(value) = env(name) else {
                return Err(format!("the secret `{name}` isn't set in the environment"));
            };
            secrets.push((name.clone(), value));
        }
        Ok(Caps { roots, secrets })
    }

    /// The secrets granted, by name, with their values.
    pub(crate) fn secrets(&self) -> &[(String, String)] {
        &self.secrets
    }

    /// The root a path names and the path inside it (`None`: the root itself), if the path is
    /// `NAME/relative/path` with no empty, `.` or `..` part, and the root is granted for
    /// writing if `write`.
    fn resolve<'s, 'p>(
        &'s self,
        path: &'p str,
        write: bool,
    ) -> Result<(&'s Root, Option<&'p str>), String> {
        let bad = |why: &str| Err(format!("the path `{path}` {why}"));
        if path.is_empty() {
            return bad("is empty");
        }
        if path.contains('\\') || path.contains('\0') {
            return bad("must have `/` between its parts");
        }
        let (name, rest) = match path.split_once('/') {
            Some((n, r)) => (n, Some(r)),
            None => (path, None),
        };
        if rest.is_some_and(|r| r.split('/').any(|p| p.is_empty() || p == "." || p == "..")) {
            return bad("has an empty, `.` or `..` part");
        }
        let Some(root) = self.roots.get(name) else {
            return bad(&format!("isn't in a granted root: there's no root `{name}`"));
        };
        if write && !root.write {
            return bad(&format!("is in `{name}`, which is granted for reading only"));
        }
        Ok((root, rest))
    }

    /// A file inside a root (not the root itself), for a service that needs one.
    fn file<'s, 'p>(&'s self, path: &'p str, write: bool) -> Result<(&'s Dir, &'p str), String> {
        match self.resolve(path, write)? {
            (root, Some(rel)) => Ok((&root.dir, rel)),
            (_, None) => Err(format!("`{path}` is a root, not a file in one")),
        }
    }

    /// The bytes of the file at `path`, at most [`MOST_BYTES`].
    pub(crate) fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        let (dir, rel) = self.file(path, false)?;
        let file = open_file(dir, rel, path, false)?;
        let mut bytes = Vec::new();
        // One byte past the most tells a file that's too large, however its size changes.
        file.take(MOST_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| failed(path, e))?;
        if bytes.len() > MOST_BYTES {
            return Err(format!(
                "`{path}` is more than 512 KiB: read it as a blob (`files::read_blob`)"
            ));
        }
        Ok(bytes)
    }

    /// Calls `each` with the file at `path`'s bytes, a chunk at a time, after `size` with its
    /// size: the caller may refuse it before any bytes are read.
    pub(crate) fn read_chunks(
        &self,
        path: &str,
        size: impl FnOnce(u64) -> Result<(), String>,
        mut each: impl FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let (dir, rel) = self.file(path, false)?;
        let mut file = open_file(dir, rel, path, false)?;
        let len = file.metadata().map_err(|e| failed(path, e))?.len();
        size(len)?;
        let mut chunk = vec![0; 1 << 20];
        loop {
            let n = file.read(&mut chunk).map_err(|e| failed(path, e))?;
            if n == 0 {
                return Ok(());
            }
            each(&chunk[..n])?;
        }
    }

    /// Writes `bytes` as the whole file at `path`, making the directories it needs.
    pub(crate) fn write(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        self.write_with(path, |f| f.write_all(bytes))
    }

    /// The entries of the directory at `path`, sorted by name. A link is listed as a link, with
    /// its own size; a name that isn't UTF-8 is listed with its bytes replaced.
    pub(crate) fn list(&self, path: &str) -> Result<Vec<Entry>, String> {
        let entries = match self.resolve(path, false)? {
            (root, None) => root.dir.entries(),
            (root, Some(rel)) => root.dir.read_dir(rel),
        }
        .map_err(|e| failed(path, e))?;
        let mut out = Vec::new();
        for e in entries {
            let e = e.map_err(|e| failed(path, e))?;
            let meta = e.metadata().map_err(|e| failed(path, e))?;
            let kind = Kind::of(e.file_type().map_err(|e| failed(path, e))?);
            let size = if kind == Kind::Dir { 0 } else { meta.len() };
            out.push(Entry { name: e.file_name().to_string_lossy().into_owned(), kind, size });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// What's at `path`: its kind and size, following a link that stays inside the root.
    pub(crate) fn stat(&self, path: &str) -> Result<Entry, String> {
        let meta = match self.resolve(path, false)? {
            (root, None) => root.dir.dir_metadata(),
            (root, Some(rel)) => root.dir.metadata(rel),
        }
        .map_err(|e| failed(path, e))?;
        let kind = Kind::of(meta.file_type());
        let size = if kind == Kind::Dir { 0 } else { meta.len() };
        Ok(Entry { name: String::new(), kind, size })
    }

    /// Removes the file, link or empty directory at `path`.
    pub(crate) fn remove(&self, path: &str) -> Result<(), String> {
        let (dir, rel) = self.file(path, true)?;
        let meta = dir.symlink_metadata(rel).map_err(|e| failed(path, e))?;
        if meta.is_dir() { dir.remove_dir(rel) } else { dir.remove_file(rel) }
            .map_err(|e| failed(path, e))
    }

    /// Removes the directory at `path` and everything in it.
    pub(crate) fn remove_all(&self, path: &str) -> Result<(), String> {
        let (dir, rel) = self.file(path, true)?;
        dir.remove_dir_all(rel).map_err(|e| failed(path, e))
    }

    /// Makes the directory at `path` and those above it that aren't there.
    pub(crate) fn make_dirs(&self, path: &str) -> Result<(), String> {
        match self.resolve(path, true)? {
            (_, None) => Ok(()),
            (root, Some(rel)) => root.dir.create_dir_all(rel).map_err(|e| failed(path, e)),
        }
    }

    /// Writes the whole file at `path` from `write`, which is given the file to write to, making
    /// the directories it needs.
    pub(crate) fn write_with(
        &self,
        path: &str,
        write: impl FnOnce(&mut dyn std::io::Write) -> std::io::Result<()>,
    ) -> Result<(), String> {
        let (dir, rel) = self.file(path, true)?;
        if let Some((parent, _)) = rel.rsplit_once('/') {
            dir.create_dir_all(parent).map_err(|e| failed(path, e))?;
        }
        let mut file = open_file(dir, rel, path, true)?;
        write(&mut file).map_err(|e| failed(path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wrela-caps-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    #[test]
    fn paths_name_a_granted_root_and_stay_in_it() {
        let dir = scratch("paths");
        std::fs::write(dir.join("a.txt"), "a").expect("write");
        let caps =
            Caps::open(&Grants::default().root("src", &dir, false), |_| None).expect("opened");
        assert_eq!(caps.read("src/a.txt").expect("read"), b"a");
        for bad in ["", "src/", "src//a.txt", "src/./a.txt", "src/../a.txt", "other/a.txt"] {
            assert!(caps.read(bad).is_err(), "{bad:?}");
        }
        let refused = caps.write("src/b.txt", b"b").expect_err("a read root");
        assert!(refused.contains("for reading only"), "{refused}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grants_are_checked_before_the_tool_starts() {
        let dir = scratch("grants");
        let twice = Grants::default().root("a", &dir, false).root("a", &dir, true);
        assert!(Caps::open(&twice, |_| None).unwrap_err().contains("twice"));
        let missing = Grants::default().root("a", dir.join("nowhere"), false);
        assert!(Caps::open(&missing, |_| None).is_err());
        let secret = Grants::default().secret("TOKEN");
        assert!(Caps::open(&secret, |_| None).unwrap_err().contains("isn't set"));
        let caps = Caps::open(&secret, |_| Some("s3cret".into())).expect("opened");
        assert!(!format!("{caps:?}").contains("s3cret"), "a secret's value is never shown");
        let mut g = Grants::default();
        assert!(g.parse("--read", "src").is_err());
        assert_eq!(g.parse("--frames", "3"), Ok(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
