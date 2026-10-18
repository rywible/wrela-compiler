//! Files and directories reached through a directory handle one part at a time, without
//! following a symbolic link anywhere on the way (`openat` with `O_NOFOLLOW`): what the tool
//! host reads and writes in a root, and what a driven build fetches from its build (language.md
//! §15.5). A link in place of a part, put there before or after a check, is refused, so nothing
//! leaves the handle's directory, and nothing inside it is reached by a link's other name. A file
//! is opened without waiting (`O_NONBLOCK`) and must be a regular file.

use cap_std::fs::Dir;
use std::io::Read;

/// `path`'s parts, `/` between them, none empty, `.` or `..`.
pub(crate) fn parts(path: &str) -> Result<Vec<&str>, String> {
    if path.is_empty() || path.contains(['\\', '\0']) {
        return Err(format!("`{path}` isn't a relative path with `/` between its parts"));
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return Err(format!("`{path}` has an empty, `.` or `..` part"));
    }
    Ok(parts)
}

#[cfg(unix)]
mod unix {
    use super::*;
    use rustix::fs::{AtFlags, Mode, OFlags};

    /// The directory `parts` lead to from `dir`, each opened without following a link; with
    /// `create`, each made if it isn't there.
    pub(crate) fn open_dir(dir: &Dir, parts: &[&str], create: bool) -> std::io::Result<Dir> {
        let mut at = dir.try_clone()?;
        for part in parts {
            if create {
                match rustix::fs::mkdirat(&at, *part, Mode::from_raw_mode(0o777)) {
                    Ok(()) => {}
                    Err(e) if e == rustix::io::Errno::EXIST => {}
                    Err(e) => return Err(e.into()),
                }
            }
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let fd = rustix::fs::openat(&at, *part, flags, Mode::empty())?;
            at = Dir::from_std_file(std::fs::File::from(fd));
        }
        Ok(at)
    }

    /// Makes the directory `name` in `dir`: whether this made it (`false`: it was there).
    pub(crate) fn make_dir(dir: &Dir, name: &str) -> std::io::Result<bool> {
        match rustix::fs::mkdirat(dir, name, Mode::from_raw_mode(0o777)) {
            Ok(()) => Ok(true),
            Err(e) if e == rustix::io::Errno::EXIST => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// The regular file `name` in `dir`, for reading, or for writing (made if it isn't there,
    /// and not emptied: the caller empties it), not followed if it's a link.
    pub(crate) fn open_file(dir: &Dir, name: &str, write: bool) -> std::io::Result<std::fs::File> {
        let mut flags = OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        flags |= if write { OFlags::WRONLY | OFlags::CREATE } else { OFlags::RDONLY };
        let file =
            std::fs::File::from(rustix::fs::openat(dir, name, flags, Mode::from_raw_mode(0o666))?);
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::other("it isn't a regular file"));
        }
        Ok(file)
    }

    /// Removes the file, link (itself) or empty directory `name` in `dir`.
    pub(crate) fn remove(dir: &Dir, name: &str, is_dir: bool) -> std::io::Result<()> {
        let flags = if is_dir { AtFlags::REMOVEDIR } else { AtFlags::empty() };
        rustix::fs::unlinkat(dir, name, flags).map_err(Into::into)
    }
}

#[cfg(unix)]
pub(crate) use unix::{make_dir, open_dir, open_file, remove};

#[cfg(not(unix))]
pub(crate) fn open_dir(_: &Dir, _: &[&str], _: bool) -> std::io::Result<Dir> {
    Err(std::io::Error::other("a confined path needs a Unix"))
}

#[cfg(not(unix))]
pub(crate) fn open_file(_: &Dir, _: &str, _: bool) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::other("a confined path needs a Unix"))
}

#[cfg(not(unix))]
pub(crate) fn make_dir(_: &Dir, _: &str) -> std::io::Result<bool> {
    Err(std::io::Error::other("a confined path needs a Unix"))
}

#[cfg(not(unix))]
pub(crate) fn remove(_: &Dir, _: &str, _: bool) -> std::io::Result<()> {
    Err(std::io::Error::other("a confined path needs a Unix"))
}

/// The bytes of the regular file at `path` in `dir`, at most `most`.
pub(crate) fn read(dir: &Dir, path: &str, most: u64) -> Result<Vec<u8>, String> {
    let parts = parts(path)?;
    let (name, above) = parts.split_last().expect("a path has a part");
    let at = open_dir(dir, above, false).map_err(|e| format!("`{path}`: {e}"))?;
    let file = open_file(&at, name, false).map_err(|e| format!("`{path}`: {e}"))?;
    let mut bytes = Vec::new();
    file.take(most.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| format!("`{path}`: {e}"))?;
    if bytes.len() as u64 > most {
        return Err(format!("`{path}` is more than {most} bytes"));
    }
    Ok(bytes)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;

    /// Writes `bytes` as the whole regular file at `path` in `dir`, making the directories it needs.
    fn write(dir: &Dir, path: &str, bytes: &[u8]) -> Result<(), String> {
        let parts = parts(path)?;
        let (name, above) = parts.split_last().expect("a path has a part");
        let at = open_dir(dir, above, true).map_err(|e| format!("`{path}`: {e}"))?;
        let mut file = open_file(&at, name, true).map_err(|e| format!("`{path}`: {e}"))?;
        file.set_len(0).map_err(|e| format!("`{path}`: {e}"))?;
        file.write_all(bytes).map_err(|e| format!("`{path}`: {e}"))
    }

    #[test]
    fn no_link_is_followed_on_the_way_or_at_the_end() {
        let base = std::env::temp_dir().join(format!("wrela-confined-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("root/sub")).expect("root");
        std::fs::write(base.join("root/user.txt"), "keep").expect("user file");
        std::os::unix::fs::symlink(base.join("root/user.txt"), base.join("root/link.txt"))
            .expect("a file link");
        std::os::unix::fs::symlink(base.join("root/sub"), base.join("root/via")).expect("dir link");
        let dir =
            Dir::open_ambient_dir(base.join("root"), cap_std::ambient_authority()).expect("dir");
        assert!(write(&dir, "link.txt", b"x").is_err(), "a link isn't written through");
        assert!(read(&dir, "via/none", 10).is_err() && write(&dir, "via/a.txt", b"x").is_err());
        assert_eq!(std::fs::read_to_string(base.join("root/user.txt")).expect("kept"), "keep");
        write(&dir, "sub/deep/a.txt", b"made").expect("a plain path");
        assert_eq!(read(&dir, "sub/deep/a.txt", 10).expect("read"), b"made");
        let _ = std::fs::remove_dir_all(&base);
    }
}
