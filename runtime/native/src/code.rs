//! Compiled code, kept from one run of a tool to the next (language.md §15.5): a module's WASM
//! compiled for an engine, as wasmtime serializes it, in a directory of the host's own
//! ([`cache_dir`]: `WRELA_CODE_CACHE`, else `~/.cache/wrela/code`, mode 0700). The tool host
//! keeps the code of a tool's own build there, and of the builds the tool drives, which it
//! compiles once in a run and gives to each build's process.
//!
//! A file's name is the SHA-256 of the module's WASM and the engine's fingerprint (wasmtime's
//! version, its settings and the machine's: [`fingerprint`]), so code is read back only for the
//! WASM and the engine it was compiled for. A file is written whole, under a name of its own,
//! then renamed into place, so a reader never finds part of one. Code not read or written for a
//! week is removed as new code is written.
//!
//! Reading compiled code back is `unsafe` in wasmtime: the bytes become executable code, so they
//! must be what `Module::serialize` wrote. Wasmtime checks that they were written by its own
//! version with this engine's settings, and refuses them if not. No tool can write the cache:
//! `wrela tool` refuses a grant whose directory is at or above it (`tool::Caps::refuse_code`),
//! and a driven build's process opens no path at all; it gets its code from the tool host,
//! which compiled it or read it from here.
#![allow(unsafe_code)]

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use wasmtime::{Engine, Module};

/// The environment variable that names the cache's directory; `off` turns the cache off.
pub const CODE_CACHE_ENV: &str = "WRELA_CODE_CACHE";

/// How long code that nothing reads or writes stays in the cache.
const KEPT: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// The SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// The cache's directory, made (mode 0700) if it's missing: `WRELA_CODE_CACHE`, else
/// `~/.cache/wrela/code`. `None` if `WRELA_CODE_CACHE` is `off`, or there's no home to keep it
/// in. Its real path, its links followed.
pub fn cache_dir() -> Result<Option<PathBuf>, String> {
    let named = std::env::var_os(CODE_CACHE_ENV).filter(|d| !d.is_empty());
    if named.as_deref() == Some(std::ffi::OsStr::new("off")) {
        return Ok(None);
    }
    let dir = match named {
        Some(d) => PathBuf::from(d),
        None => {
            let Some(home) = std::env::var_os("HOME").filter(|d| !d.is_empty()) else {
                return Ok(None);
            };
            PathBuf::from(home).join(".cache/wrela/code")
        }
    };
    let failed = |e: std::io::Error| format!("the code cache ({}): {e}", dir.display());
    let mut make = std::fs::DirBuilder::new();
    make.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut make, 0o700);
    make.create(&dir).map_err(failed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).map_err(failed)?.permissions().mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .map_err(failed)?;
        }
    }
    dir.canonicalize().map(Some).map_err(failed)
}

/// What wasmtime's compiled code depends on for `engine`: its version, its settings and the
/// machine it compiles for, as a hex number.
pub fn fingerprint(engine: &Engine) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    engine.precompile_compatibility_hash().hash(&mut h);
    format!("{:016x}", h.finish())
}

/// `wasm` compiled for `engine`, serialized.
pub fn compile(engine: &Engine, wasm: &[u8]) -> Result<Vec<u8>, String> {
    let module = Module::new(engine, wasm).map_err(|e| format!("{e:#}"))?;
    module.serialize().map_err(|e| format!("{e:#}"))
}

/// The module whose code `bytes` is, for `engine`: bytes that [`compile`] gave, for this
/// engine's settings, whether from the cache, the tool host, or this process.
pub fn load(engine: &Engine, bytes: &[u8]) -> Result<Module, String> {
    // SAFETY: the bytes are what `Module::serialize` wrote ([`compile`]): read from the cache,
    // which no tool can write (see the module's documentation), or sent by the tool host that
    // compiled or read them. Wasmtime refuses code from another version or configuration.
    unsafe { Module::deserialize(engine, bytes) }.map_err(|e| format!("{e:#}"))
}

/// The cache of compiled code: its directory, opened once, and its path (which names it).
/// Every artifact is reached through the opened directory, by its name alone, following no
/// link.
#[derive(Clone, Debug)]
pub struct Cache {
    dir: PathBuf,
    handle: std::sync::Arc<cap_std::fs::Dir>,
}

impl Cache {
    /// The cache in `dir` (a [`cache_dir`]: its real path), opened now, following no link at its
    /// end.
    pub fn open(dir: PathBuf) -> Result<Cache, String> {
        use rustix::fs::{Mode, OFlags};
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = rustix::fs::open(&dir, flags, Mode::empty())
            .map_err(|e| format!("the code cache ({}): {e}", dir.display()))?;
        let handle = cap_std::fs::Dir::from_std_file(std::fs::File::from(fd));
        Ok(Cache { dir, handle: std::sync::Arc::new(handle) })
    }

    /// Its directory's path.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Its directory's device and inode, as opened.
    #[allow(clippy::unnecessary_cast)]
    pub fn identity(&self) -> Result<(u64, u64), String> {
        let st = rustix::fs::fstat(&*self.handle).map_err(|e| format!("the code cache: {e}"))?;
        Ok((st.st_dev as u64, st.st_ino as u64))
    }

    fn name(engine: &Engine, sha: &[u8; 32]) -> String {
        let hex: String = sha.iter().map(|b| format!("{b:02x}")).collect();
        format!("{hex}-{}.cwasm", fingerprint(engine))
    }

    /// The code kept for the WASM whose SHA-256 is `sha`, compiled for `engine`, if there is
    /// any: it's kept a week more. Only a regular file of the user's own, with no other name (no
    /// hard link elsewhere could have changed it), reached without a link, is read back; any other
    /// entry by that name is removed, and the code is compiled again.
    pub fn get(&self, engine: &Engine, sha: &[u8; 32]) -> Option<Vec<u8>> {
        use rustix::fs::{Mode, OFlags};
        let name = Cache::name(engine, sha);
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let opened = rustix::fs::openat(&*self.handle, name.as_str(), flags, Mode::empty());
        let fd = match opened {
            Ok(fd) => fd,
            Err(e) if e == rustix::io::Errno::NOENT => return None,
            Err(_) => {
                self.remove(&name);
                return None;
            }
        };
        let trusted = rustix::fs::fstat(&fd).is_ok_and(|st| {
            rustix::fs::FileType::from_raw_mode(st.st_mode) == rustix::fs::FileType::RegularFile
                && st.st_nlink == 1
                && st.st_uid == rustix::process::geteuid().as_raw()
        });
        if !trusted {
            self.remove(&name);
            return None;
        }
        let mut file = std::fs::File::from(fd);
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes).ok()?;
        let _ = file.set_modified(std::time::SystemTime::now());
        Some(bytes)
    }

    /// Removes the entry `name`, whatever it is (a link itself, not what it names).
    fn remove(&self, name: &str) {
        let _ = rustix::fs::unlinkat(&*self.handle, name, rustix::fs::AtFlags::empty());
    }

    /// Keeps `bytes`, the code of the WASM whose SHA-256 is `sha`, compiled for `engine` (best
    /// effort: a cache that can't be written only costs the next run a compile). They're written
    /// to a new file of this write's own (made here, never one that was there), which then takes
    /// the name's place.
    pub fn put(&self, engine: &Engine, sha: &[u8; 32], bytes: &[u8]) {
        use rustix::fs::{Mode, OFlags};
        static WRITES: AtomicU64 = AtomicU64::new(0);
        let name = Cache::name(engine, sha);
        let n = WRITES.fetch_add(1, Ordering::Relaxed);
        let tmp = format!("{name}.{}-{n}.tmp", std::process::id());
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let written =
            rustix::fs::openat(&*self.handle, tmp.as_str(), flags, Mode::from_raw_mode(0o600))
                .map_err(std::io::Error::from)
                .and_then(|fd| std::io::Write::write_all(&mut std::fs::File::from(fd), bytes))
                .and_then(|()| {
                    rustix::fs::renameat(&*self.handle, tmp.as_str(), &*self.handle, name.as_str())
                        .map_err(Into::into)
                });
        if written.is_ok() {
            self.remove_stale();
        } else {
            self.remove(&tmp);
        }
    }

    /// Removes the code that nothing has read or written for [`KEPT`].
    fn remove_stale(&self) {
        for e in self.handle.entries().into_iter().flatten().flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.into_std().elapsed().is_ok_and(|a| a > KEPT));
            let name = e.file_name();
            let name = name.to_string_lossy();
            if old && (name.ends_with(".cwasm") || name.ends_with(".tmp")) {
                self.remove(&name);
            }
        }
    }

    /// The module of `wasm` for `engine`: its code read back from the cache, else compiled and
    /// kept. A fresh compile is returned directly, without deserializing its saved copy.
    pub fn module(&self, engine: &Engine, wasm: &[u8]) -> Result<Module, String> {
        let sha = sha256(wasm);
        if let Some(bytes) = self.get(engine, &sha)
            && let Ok(m) = load(engine, &bytes)
        {
            return Ok(m);
        }
        let module = Module::new(engine, wasm).map_err(|e| format!("{e:#}"))?;
        if let Ok(bytes) = module.serialize() {
            self.put(engine, &sha, &bytes);
        }
        Ok(module)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_comes_back_for_its_wasm_and_its_engine_alone() {
        let dir = std::env::temp_dir().join(format!("wrela-code-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let cache = Cache::open(dir.clone()).expect("the cache");
        let wasm =
            wat::parse_str("(module (func (export \"f\") (result i32) i32.const 7))").expect("wat");
        let plain = Engine::default();
        let mut config = wasmtime::Config::new();
        config.epoch_interruption(true);
        let other = Engine::new(&config).expect("engine");
        assert_ne!(fingerprint(&plain), fingerprint(&other), "the settings are in the name");
        assert!(cache.get(&plain, &sha256(&wasm)).is_none());
        let fresh = cache.module(&plain, &wasm).expect("compiled and kept");
        assert!(fresh.get_export("f").is_some(), "the fresh module has the WASM's export");
        assert!(cache.get(&plain, &sha256(&wasm)).is_some(), "kept for its WASM");
        let warm = cache.module(&plain, &wasm).expect("loaded from the cache");
        assert!(warm.get_export("f").is_some(), "the cached module has the same export");
        assert!(cache.get(&other, &sha256(&wasm)).is_none(), "not for another engine");
        assert!(cache.get(&plain, &sha256(b"other")).is_none(), "nor for other WASM");
        let names: Vec<_> = std::fs::read_dir(&dir).expect("read").flatten().collect();
        assert_eq!(names.len(), 1, "no temporary file is left");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
