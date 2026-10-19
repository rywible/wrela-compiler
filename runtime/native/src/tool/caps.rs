//! What a tool may reach (language.md §15.5): the grants of its command line, opened once into
//! one capability context ([`Caps`]) that every service is given. A root is a directory opened
//! as a handle (`cap_std::fs::Dir`), and every path is opened relative to it, one part at a time,
//! so no path, `..` or symbolic link leaves it, even a link put in place of a directory after a
//! check.
//!
//! A file is opened without waiting (`O_NONBLOCK`), so a FIFO with nothing at its other end can't
//! hold a worker, and the opened handle must be a regular file: a FIFO, a socket or a device is
//! refused, by the handle's own type, not the path's.
//!
//! Hosts (`--net HOST`) and programs (`--allow-run NAME[=PATH]`) are granted by name too. A
//! program's executable is found when the grants are opened, on the `PATH` of that moment, and
//! kept as an absolute path, so nothing later changes which file runs. A tool test's hosts are
//! fakes, served from its `scratch` root, and its programs tool packages (`tool:DIR`).

use cap_std::ambient_authority;
use cap_std::fs::{Dir, File, FileType, OpenOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The most bytes a whole file read or written at once holds (`std::tool::MOST_BYTES`): a larger
/// one goes through a blob.
pub const MOST_BYTES: usize = 512 * 1024;

/// What the command line grants a tool, before anything is opened: roots by name (`--read
/// NAME=DIR`, `--write NAME=DIR`), secrets by name (`--secret NAME`), hosts (`--net HOST`) and
/// programs (`--allow-run NAME[=PATH]`).
#[derive(Clone, Debug, Default)]
pub struct Grants {
    /// Each root: its name, its directory, and whether the tool may change it.
    pub roots: Vec<(String, PathBuf, bool)>,
    /// The environment variables the tool may read, by name.
    pub secrets: Vec<String>,
    /// The hosts `std::tool::http` may reach, over HTTPS.
    pub net: Vec<String>,
    /// The hosts a tool test's fake sites stand in for, served from `scratch/sites/<host>/`.
    pub fake_net: Vec<String>,
    /// The programs `std::tool::run` may run: each name and where it's found.
    pub programs: Vec<(String, ProgramGrant)>,
    /// The `wrela` executable that runs a `tool:` program's package (`wrela tool`).
    pub tool_runner: Option<PathBuf>,
    /// Whether `std::tool::browser` may start Chrome (`--browser`).
    pub browser: bool,
    /// Roots opened already, by name: [`Caps::open`] keeps each handle instead of opening its
    /// directory again by its path, so what was checked through the handle is what's granted.
    pub opened: Vec<(String, Opened)>,
}

/// A directory, opened: a handle that a root can be granted as ([`Grants::opened`]).
#[derive(Clone, Debug)]
pub struct Opened(std::sync::Arc<Dir>);

impl Opened {
    /// The directory at `path`, opened by its path: a directory the command trusts, such as a
    /// workspace's.
    pub fn at(path: &std::path::Path) -> Result<Opened, String> {
        let dir = Dir::open_ambient_dir(path, ambient_authority())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Opened(std::sync::Arc::new(dir)))
    }

    /// The directory at `rel` (parts separated by `/`, or `.` for this one) inside this one,
    /// each part opened through the one before without following a link, and with `make`, made
    /// if it's missing. So whatever replaces a part, before or after, nothing outside is reached
    /// or made.
    pub fn within(&self, rel: &str, make: bool) -> Result<Opened, String> {
        let dir = if rel == "." {
            self.0.try_clone().map_err(|e| e.to_string())?
        } else {
            let parts = crate::confined::parts(rel)?;
            crate::confined::open_dir(&self.0, &parts, make).map_err(|e| format!("`{rel}`: {e}"))?
        };
        Ok(Opened(std::sync::Arc::new(dir)))
    }
}

/// Where a granted program is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgramGrant {
    /// `--allow-run NAME`: the file `NAME` in a directory of `PATH`, found when the grants are
    /// opened.
    OnPath,
    /// `--allow-run NAME=PATH`: that executable.
    At(PathBuf),
    /// `--allow-run NAME=tool:DIR`: the tool build of the package at `DIR`, which `wrela tool`
    /// runs in a process of its own.
    Tool(PathBuf),
}

/// A granted program, found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Program {
    /// An executable, by its absolute path.
    Native(PathBuf),
    /// A tool package's tool build, run by `runner` (`wrela tool`).
    Tool { runner: PathBuf, package: PathBuf },
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

    /// Grants the host `host`, for HTTPS.
    pub fn net(mut self, host: &str) -> Grants {
        self.net.push(host.to_string());
        self
    }

    /// Grants the program `name`, found as `grant` says.
    pub fn program(mut self, name: &str, grant: ProgramGrant) -> Grants {
        self.programs.push((name.to_string(), grant));
        self
    }

    /// Reads one grant of a command line: `--read NAME=DIR`, `--write NAME=DIR`, `--secret
    /// NAME`, `--net HOST` or `--allow-run NAME[=PATH]` (`=tool:DIR` for a tool package), as the
    /// flag and its value. `Ok(false)` if `flag` isn't a grant's. (`--browser` takes no value:
    /// it sets [`Grants::browser`].)
    pub fn parse(&mut self, flag: &str, value: &str) -> Result<bool, String> {
        let write = match flag {
            "--read" => false,
            "--write" => true,
            "--secret" => {
                self.secrets.push(value.to_string());
                return Ok(true);
            }
            "--net" => {
                self.net.push(value.to_string());
                return Ok(true);
            }
            "--allow-run" => {
                let grant = match value.split_once('=') {
                    None => (value, ProgramGrant::OnPath),
                    Some((name, at)) => match at.strip_prefix("tool:") {
                        Some(dir) => (name, ProgramGrant::Tool(PathBuf::from(dir))),
                        None => (name, ProgramGrant::At(PathBuf::from(at))),
                    },
                };
                self.programs.push((grant.0.to_string(), grant.1));
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

/// A root, opened: its handle, and its directory's path, for a program's working directory.
struct Root {
    dir: Dir,
    path: PathBuf,
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
    net: BTreeSet<String>,
    fake_net: BTreeSet<String>,
    programs: BTreeMap<String, Program>,
    /// `PATH` when the grants were opened: the baseline a program's environment starts from.
    path: String,
    /// The `wrela` that runs `tool:` programs and launches supervised ones.
    runner: Option<PathBuf>,
    browser: bool,
}

impl std::fmt::Debug for Caps {
    /// Names the roots, the secrets, the hosts and the programs, never the secrets' values.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let roots: Vec<(&String, bool)> = self.roots.iter().map(|(n, r)| (n, r.write)).collect();
        let secrets: Vec<&String> = self.secrets.iter().map(|(n, _)| n).collect();
        f.debug_struct("Caps")
            .field("roots", &roots)
            .field("secrets", &secrets)
            .field("net", &self.net)
            .field("fake_net", &self.fake_net)
            .field("programs", &self.programs)
            .field("browser", &self.browser)
            .finish()
    }
}

/// Whether `host` can name a host: lowercase letters, digits, `-` and `.`, no part empty, and
/// then `:` and a port from 1 if `port` allows one.
pub(crate) fn valid_host(host: &str, port: bool) -> bool {
    let (name, at) = match host.split_once(':') {
        Some((name, at)) if port => (name, Some(at)),
        _ => (host, None),
    };
    !name.is_empty()
        && name.split('.').all(|p| !p.is_empty())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && at.is_none_or(|p| p.parse::<u16>().is_ok_and(|n| n > 0) && !p.starts_with('0'))
}

/// Whether the file at `path` is one the user may run: a regular file with an execute bit.
fn executable(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    let runs = |m: &std::fs::Metadata| {
        std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0
    };
    #[cfg(not(unix))]
    let runs = |_: &std::fs::Metadata| true;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && runs(&m))
}

/// Finds program `name` as `grant` says, given `PATH`'s value then: its absolute path.
fn find_program(
    name: &str,
    grant: &ProgramGrant,
    path: &str,
    runner: Option<&PathBuf>,
) -> Result<Program, String> {
    let found = match grant {
        ProgramGrant::OnPath => {
            let found = std::env::split_paths(path)
                .filter(|d| d.is_absolute())
                .map(|d| d.join(name))
                .find(|p| executable(p));
            found.ok_or_else(|| format!("the program `{name}` isn't on PATH"))?
        }
        ProgramGrant::At(at) => {
            if !executable(at) {
                return Err(format!(
                    "the program `{name}` ({}) isn't an executable file",
                    at.display()
                ));
            }
            at.clone()
        }
        ProgramGrant::Tool(dir) => {
            let Some(runner) = runner else {
                return Err(format!("the program `{name}` is a tool, and nothing here runs one"));
            };
            let package = dir.canonicalize().map_err(|e| {
                format!("the program `{name}`'s tool package ({}): {e}", dir.display())
            })?;
            if !package.join("wrela.toml").is_file() {
                return Err(format!(
                    "the program `{name}`'s tool package ({}) has no wrela.toml",
                    dir.display()
                ));
            }
            return Ok(Program::Tool { runner: runner.clone(), package });
        }
    };
    // Absolute, not canonical: a link keeps its name, which a program may go by (`argv[0]`).
    let absolute = std::path::absolute(&found)
        .map_err(|e| format!("the program `{name}` ({}): {e}", found.display()))?;
    Ok(Program::Native(absolute))
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
            let kept = grants.opened.iter().find(|(n, _)| n == name);
            let (opened, path) = match kept {
                // Opened and checked already: the handle is the root, and its path names it for
                // a program's working directory.
                Some((_, o)) => {
                    (o.0.try_clone().map_err(|e| format!("the root `{name}`: {e}"))?, dir.clone())
                }
                None => (
                    Dir::open_ambient_dir(dir, ambient_authority())
                        .map_err(|e| format!("the root `{name}` ({}): {e}", dir.display()))?,
                    dir.canonicalize()
                        .map_err(|e| format!("the root `{name}` ({}): {e}", dir.display()))?,
                ),
            };
            roots.insert(name.clone(), Root { dir: opened, path, write: *write });
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
        // A host of the network may name its port; a fake site is a name alone.
        let hosts = |given: &[String], port: bool| -> Result<BTreeSet<String>, String> {
            given
                .iter()
                .map(|h| {
                    if valid_host(h, port) {
                        Ok(h.clone())
                    } else {
                        Err(format!(
                            "`{h}` can't name a host: lowercase letters, digits, `-` and `.`, then `:PORT` if it has one"
                        ))
                    }
                })
                .collect()
        };
        let net = hosts(&grants.net, true)?;
        let fake_net = hosts(&grants.fake_net, false)?;
        let path = env("PATH").unwrap_or_default();
        let mut programs = BTreeMap::new();
        for (name, grant) in &grants.programs {
            if !valid_name(name) {
                return Err(format!(
                    "`{name}` can't name a program: use letters, digits, `_`, `-` and `.`"
                ));
            }
            if programs.contains_key(name) {
                return Err(format!("the program `{name}` is granted twice"));
            }
            let found = find_program(name, grant, &path, grants.tool_runner.as_ref())?;
            programs.insert(name.clone(), found);
        }
        Ok(Caps {
            roots,
            secrets,
            net,
            fake_net,
            programs,
            path,
            runner: grants.tool_runner.clone(),
            browser: grants.browser,
        })
    }

    /// Whether the tool may start a browser (`--browser`).
    pub(crate) fn browser(&self) -> bool {
        self.browser
    }

    /// The secrets granted, by name, with their values.
    pub(crate) fn secrets(&self) -> &[(String, String)] {
        &self.secrets
    }

    /// The secret granted as `name`'s value.
    pub(crate) fn secret(&self, name: &str) -> Option<&str> {
        self.secrets.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// Whether `host` is granted (`--net`), and whether as a fake site: `None` if it isn't.
    pub(crate) fn host(&self, host: &str) -> Option<bool> {
        if self.fake_net.contains(host) {
            Some(true)
        } else {
            self.net.contains(host).then_some(false)
        }
    }

    /// The program granted as `name`.
    pub(crate) fn program(&self, name: &str) -> Result<&Program, String> {
        self.programs.get(name).ok_or_else(|| {
            format!("the program `{name}` isn't granted: `--allow-run {name}` grants it")
        })
    }

    /// The `wrela` that launches a supervised program, if there is one.
    pub(crate) fn runner(&self) -> Option<&std::path::Path> {
        self.runner.as_deref()
    }

    /// `PATH` when the grants were opened.
    pub(crate) fn path_baseline(&self) -> &str {
        &self.path
    }

    /// The directory of write root `name`, for a program to work in.
    pub(crate) fn write_root_dir(&self, name: &str) -> Result<&std::path::Path, String> {
        match self.roots.get(name) {
            Some(r) if r.write => Ok(&r.path),
            Some(_) => Err(format!(
                "`{name}` is granted for reading only: a program works in a write root"
            )),
            None => Err(format!("there's no root `{name}` for a program to work in")),
        }
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

    /// The directory at `path`, by its real path, if it's a directory inside its root (the root
    /// itself for `NAME`), and the root is granted for writing if `write`: what the toolchain
    /// and drive services give the compiler and the native host, which read through `reach`.
    pub(crate) fn dir_path(&self, path: &str, write: bool) -> Result<PathBuf, String> {
        let (root, rel) = self.resolve(path, write)?;
        let Some(rel) = rel else { return Ok(root.path.clone()) };
        // Opened through the root's handle, so a link can't lead it out.
        let meta = root.dir.metadata(rel).map_err(|e| failed(path, e))?;
        if !meta.is_dir() {
            return Err(format!("`{path}` isn't a directory"));
        }
        let real = root.path.join(rel).canonicalize().map_err(|e| failed(path, e))?;
        if !real.starts_with(&root.path) {
            return Err(format!("`{path}` leads outside its root"));
        }
        Ok(real)
    }

    /// The directory at `path` (a root path, or a root), opened through its root's handle one
    /// part at a time, following no link: its real path, and the handle, which later reads go
    /// through, so a link put in its place after this leads nowhere.
    pub(crate) fn open_dir(&self, path: &str) -> Result<(PathBuf, Dir), String> {
        let (root, rel) = self.resolve(path, false)?;
        let parts: Vec<&str> = rel.map_or_else(Vec::new, |r| r.split('/').collect());
        let dir =
            crate::confined::open_dir(&root.dir, &parts, false).map_err(|e| failed(path, e))?;
        let real = parts.iter().fold(root.path.clone(), |p, part| p.join(part));
        Ok((real, dir))
    }

    /// The directory above the file at `path` in a write root, opened one part at a time
    /// through the root's handle without following a link (made, with `create`), and the
    /// file's name: what output that must not be written through a link is written in.
    fn parent_nofollow(&self, path: &str, create: bool) -> Result<(Dir, String), String> {
        let (dir, rel) = self.file(path, true)?;
        let parts = crate::confined::parts(rel)?;
        let (name, above) = parts.split_last().expect("a path has a part");
        let at = crate::confined::open_dir(dir, above, create).map_err(|e| failed(path, e))?;
        Ok((at, name.to_string()))
    }

    /// Makes the directories at `path` in a write root, one part at a time, following no link
    /// (a link in place of one stops it): those above it, and `path` itself if `itself`. Each
    /// this call made is added to `made`, as a root path, in the order made. The last one,
    /// opened.
    pub(crate) fn make_dirs_tracked(
        &self,
        path: &str,
        itself: bool,
        made: &mut Vec<String>,
    ) -> Result<Dir, String> {
        let (root, rel) = self.resolve(path, true)?;
        let parts = match rel {
            Some(rel) => crate::confined::parts(rel)?,
            None if itself => Vec::new(),
            None => return Err(format!("`{path}` is a root, not a file in one")),
        };
        let upto = if itself { parts.len() } else { parts.len().saturating_sub(1) };
        let mut at = root.dir.try_clone().map_err(|e| failed(path, e))?;
        let mut prefix = path.split('/').next().unwrap_or_default().to_string();
        for part in &parts[..upto] {
            prefix = format!("{prefix}/{part}");
            if crate::confined::make_dir(&at, part).map_err(|e| failed(path, e))? {
                made.push(prefix.clone());
            }
            at = crate::confined::open_dir(&at, &[part], false).map_err(|e| failed(path, e))?;
        }
        Ok(at)
    }

    /// Opens the regular file at `path` in a write root to write it, making the directories it
    /// needs (each made added to `made`) and following no link: made if it isn't there, and not
    /// emptied, so the caller can count it as its own before it changes it.
    pub(crate) fn open_output(
        &self,
        path: &str,
        made: &mut Vec<String>,
    ) -> Result<std::fs::File, String> {
        let at = self.make_dirs_tracked(path, false, made)?;
        let name = path.rsplit('/').next().unwrap_or_default();
        crate::confined::open_file(&at, name, true).map_err(|e| failed(path, e))
    }

    /// Removes the empty directory at `path` in a write root, following no link.
    pub(crate) fn remove_dir_nofollow(&self, path: &str) -> Result<(), String> {
        if self.kind_nofollow(path)? != Some(Kind::Dir) {
            return Ok(());
        }
        let (at, name) = self.parent_nofollow(path, false)?;
        crate::confined::remove(&at, &name, true).map_err(|e| failed(path, e))
    }

    /// Whether `path` is in a write root (or is one).
    pub(crate) fn writable(&self, path: &str) -> Result<(), String> {
        self.resolve(path, true).map(|_| ())
    }

    /// What's at `path` in a write root, not following a link anywhere on the way: `None` if
    /// nothing is, `Link` if a part of it is a link (so nothing is reached through it).
    pub(crate) fn kind_nofollow(&self, path: &str) -> Result<Option<Kind>, String> {
        let (dir, rel) = self.file(path, true)?;
        let parts = crate::confined::parts(rel)?;
        let mut at = dir.try_clone().map_err(|e| failed(path, e))?;
        for (k, part) in parts.iter().enumerate() {
            let meta = match at.symlink_metadata(part) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(failed(path, e)),
            };
            let kind = Kind::of(meta.file_type());
            if k + 1 == parts.len() || kind != Kind::Dir {
                return Ok(Some(kind));
            }
            at = crate::confined::open_dir(&at, &[part], false).map_err(|e| failed(path, e))?;
        }
        unreachable!("a path has a part")
    }

    /// The bytes of the regular file at `path` in a write root, at most `most`, following no
    /// link: `None` if nothing is there.
    pub(crate) fn read_nofollow(&self, path: &str, most: u64) -> Result<Option<Vec<u8>>, String> {
        if self.kind_nofollow(path)?.is_none() {
            return Ok(None);
        }
        let (dir, rel) = self.file(path, true)?;
        crate::confined::read(dir, rel, most).map(Some)
    }

    /// The bytes of the regular file at `path` in any root, at most `most`, reached one part at
    /// a time following no link.
    pub(crate) fn read_confined(&self, path: &str, most: u64) -> Result<Vec<u8>, String> {
        let (dir, rel) = self.file(path, false)?;
        crate::confined::read(dir, rel, most)
    }

    /// Replaces the regular file at `path` in a write root with `bytes`, following no link: they
    /// go to a new file beside it, which then takes its place, so a reader sees the old file or
    /// the whole new one, and a failed write leaves the old one. `before` is asked right before
    /// the new file takes the old one's place: its error leaves the old file, and is the error.
    pub(crate) fn replace_nofollow(
        &self,
        path: &str,
        bytes: &[u8],
        before: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if self.kind_nofollow(path)? != Some(Kind::File) {
            return Err(format!("`{path}` isn't a regular file reached without a link"));
        }
        let (at, name) = self.parent_nofollow(path, false)?;
        static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = format!(".{name}.tmp-{}-{n}", std::process::id());
        let written = (|| {
            let mut file = crate::confined::open_file(&at, &tmp, true)?;
            file.set_len(0)?;
            file.write_all(bytes)?;
            file.sync_all()
        })()
        .map_err(|e| failed(path, e))
        .and_then(|()| before())
        .and_then(|()| crate::confined::rename(&at, &tmp, &name).map_err(|e| failed(path, e)));
        if written.is_err() {
            let _ = crate::confined::remove(&at, &tmp, false);
        }
        written
    }

    /// The root path (`NAME/rel`) of the real path `real` (with no `.` or `..` part): in the
    /// deepest root whose directory holds it, a write root if `write`.
    pub(crate) fn root_path_of(&self, real: &Path, write: bool) -> Result<String, String> {
        let mut best: Option<(&str, PathBuf, usize)> = None;
        for (name, r) in &self.roots {
            if write && !r.write {
                continue;
            }
            let dir = r.path.canonicalize().unwrap_or_else(|_| r.path.clone());
            if let Ok(rest) = real.strip_prefix(&dir) {
                let depth = dir.components().count();
                if best.as_ref().is_none_or(|(_, _, d)| depth > *d) {
                    best = Some((name, rest.to_path_buf(), depth));
                }
            }
        }
        let Some((name, rest, _)) = best else {
            let kind = if write { "write root" } else { "root" };
            return Err(format!("`{}` isn't in a granted {kind}", real.display()));
        };
        let mut out = name.to_string();
        for c in rest.components() {
            match c {
                std::path::Component::Normal(part) => {
                    out.push('/');
                    out.push_str(&part.to_string_lossy());
                }
                _ => return Err(format!("`{}` isn't a plain path", real.display())),
            }
        }
        Ok(out)
    }

    /// Removes the regular file at `path` in a write root, if one is there, following no link:
    /// a link or a directory there is left.
    pub(crate) fn remove_file_nofollow(&self, path: &str) -> Result<(), String> {
        if self.kind_nofollow(path)? != Some(Kind::File) {
            return Ok(());
        }
        let (at, name) = self.parent_nofollow(path, false)?;
        crate::confined::remove(&at, &name, false).map_err(|e| failed(path, e))
    }

    /// Every root's real path and a handle of its own to its directory, read roots and write
    /// roots alike: where the toolchain service lets the compiler read.
    pub(crate) fn readable(&self) -> Result<Vec<(PathBuf, Dir)>, String> {
        self.roots
            .iter()
            .map(|(name, r)| {
                let dir = r.dir.try_clone().map_err(|e| format!("the root `{name}`: {e}"))?;
                Ok((r.path.clone(), dir))
            })
            .collect()
    }

    /// The bytes of the regular file at `path`, at most `most`: more is an error.
    pub(crate) fn read_most(&self, path: &str, most: u64) -> Result<Vec<u8>, String> {
        let (dir, rel) = self.file(path, false)?;
        let file = open_file(dir, rel, path, false)?;
        let mut bytes = Vec::new();
        file.take(most + 1).read_to_end(&mut bytes).map_err(|e| failed(path, e))?;
        if bytes.len() as u64 > most {
            return Err(format!("`{path}` is more than {most} bytes"));
        }
        Ok(bytes)
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

    #[test]
    fn hosts_and_programs_are_granted_by_name_and_found_once() {
        let mut g = Grants::default();
        for (flag, value) in [
            ("--net", "game.example"),
            ("--net", "127.0.0.1:8443"),
            ("--allow-run", "sh"),
            ("--allow-run", "shell=/bin/sh"),
        ] {
            assert_eq!(g.parse(flag, value), Ok(true), "{flag} {value}");
        }
        let caps = Caps::open(&g, |n| (n == "PATH").then(|| "/nowhere:/bin".into())).expect("ok");
        assert_eq!(caps.host("game.example"), Some(false));
        assert_eq!(caps.host("127.0.0.1:8443"), Some(false));
        assert_eq!(caps.host("127.0.0.1"), None, "a host is granted with its port, exactly");
        let Ok(Program::Native(sh)) = caps.program("sh") else { panic!("sh") };
        assert_eq!(sh, &PathBuf::from("/bin/sh"), "found on PATH, once");
        assert!(caps.program("ls").is_err(), "only what's granted");
        assert_eq!(caps.path_baseline(), "/nowhere:/bin");
        for bad in ["Game.example", "a..b", "a.example:0", "a.example:99999", "user@a.example"] {
            let g = Grants::default().net(bad);
            assert!(Caps::open(&g, |_| None).is_err(), "{bad}");
        }
        let missing = Grants::default().program("sh", ProgramGrant::OnPath);
        assert!(Caps::open(&missing, |_| Some("/nowhere".into())).is_err(), "not on PATH");
        let tool = Grants::default().program("t", ProgramGrant::Tool(PathBuf::from("/")));
        assert!(Caps::open(&tool, |_| None).unwrap_err().contains("nothing here runs one"));
    }
}
