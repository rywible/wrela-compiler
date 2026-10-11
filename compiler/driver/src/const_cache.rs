//! Constants cached by their inputs, between builds (language.md §10). A computed constant is
//! lowered into a module of its own: the code it runs, the values of the constants it reads and
//! the files it embeds, nothing else. The module's WASM is the constant's key, with the fuel it
//! may use, so a constant is computed again only when one of those changes: an edit to a
//! function it doesn't call leaves its module, and its key, as they were.
//!
//! The cache is a directory of files in the package's `build` directory (`build/consts`), one
//! per key, each a constant's value ([`encode`]), or the directory `WRELA_CONST_CACHE` names,
//! which every package and checkout that sets it share. A file is written whole to a temporary
//! name and renamed into place, so two builds at once never read half of one. Files no build has
//! used for a week are removed.

use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use wrela_ir as ir;
use wrela_lower::Value;

/// The format's first bytes and version: a file of another version is a miss.
const MAGIC: &[u8; 8] = b"wrelac\x00\x02";

/// How long an entry no build has used is kept.
const KEEP_FOR: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// The cache's directory for the package at `root`: `WRELA_CONST_CACHE` when it's set, else
/// its `build/consts`. One directory can serve every package and checkout, as a key is all a
/// value depends on: a fresh checkout reads the constants another one computed.
pub fn dir_of(root: &Path) -> Option<PathBuf> {
    dir_in(root, std::env::var_os("WRELA_CONST_CACHE"))
}

fn dir_in(root: &Path, shared: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let dir = match shared.filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => root.canonicalize().ok()?.join("build").join("consts"),
    };
    safe(&dir)?;
    Some(dir)
}

/// Refuses a link in a cache path. For the package-owned `<package>/build/consts`, checks from
/// the package; an explicitly configured shared directory is itself the ownership root. This
/// is a preflight; [`crate::ensure_no_symlink_path`] documents its unavoidable replacement race.
fn safe(dir: &Path) -> Option<()> {
    let root = package_root(dir).unwrap_or(dir);
    crate::ensure_no_symlink_path(root, dir).ok()
}

fn safe_file(dir: &Path, path: &Path) -> Option<()> {
    safe(dir)?;
    crate::ensure_no_symlink_path(dir, path).ok()
}

fn package_root(dir: &Path) -> Option<&Path> {
    let build = dir.parent()?;
    (dir.file_name()? == "consts" && build.file_name()? == "build").then(|| build.parent())?
}

/// A constant's key: its module's WASM and the fuel it may use, with the compiler's version.
/// How many threads computed it isn't part of it: results don't depend on them (§6.12).
pub fn key(wasm: &[u8], fuel: u64) -> String {
    let mut h = Sha256::new();
    h.update(MAGIC);
    h.update(env!("CARGO_PKG_VERSION").as_bytes());
    h.update(fuel.to_le_bytes());
    h.update((wasm.len() as u64).to_le_bytes());
    h.update(wasm);
    let mut key = String::with_capacity(64);
    for b in h.finalize() {
        write!(key, "{b:02x}").expect("write a cache key");
    }
    key
}

/// The value cached under `key` in `dir`, if there's one that reads back whole. A hit marks the
/// file used now, so it's kept.
pub fn get(dir: &Path, key: &str) -> Option<Value> {
    let path = dir.join(format!("{key}.wcv"));
    safe_file(dir, &path)?;
    let bytes = std::fs::read(&path).ok()?;
    let value = decode(&bytes)?;
    // Touched, so cleaning keeps it.
    let _ = std::fs::File::options()
        .append(true)
        .open(&path)
        .and_then(|f| f.set_modified(std::time::SystemTime::now()));
    Some(value)
}

/// Stores `value` under `key` in `dir`. Failing to is no error: the next build computes it again.
pub fn put(dir: &Path, key: &str, value: &Value) {
    if safe(dir).is_none() {
        return;
    }
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if safe(dir).is_none() {
        return;
    }
    let bytes = encode(value);
    // Unique across machines too, which share a directory but not their process ids.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let tmp = dir.join(format!("{key}.{}-{nanos}.tmp", std::process::id()));
    if safe_file(dir, &tmp).is_none() {
        return;
    }
    if std::fs::write(&tmp, &bytes).is_ok()
        && std::fs::rename(&tmp, dir.join(format!("{key}.wcv"))).is_err()
    {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Removes the entries in `dir` that no build has used for [`KEEP_FOR`], and temporary files
/// left by a build that stopped.
pub fn clean(dir: &Path) {
    if safe(dir).is_none() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = std::time::SystemTime::now();
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(temporary) = cache_name(&name) else { continue };
        if !e.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let path = e.path();
        let age = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .unwrap_or_default();
        if (!temporary && age > KEEP_FOR) || (temporary && age.as_secs() > 3600) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Whether `name` belongs to this cache: a 64-hex-digit key and `.wcv` (false), or its
/// `<key>.<pid>-<nanos>.tmp` temporary (true). The earlier `<key>.<pid>.tmp` form is owned too,
/// so an old interrupted build's temporary can still be removed. Anything else is somebody
/// else's.
fn cache_name(name: &std::ffi::OsStr) -> Option<bool> {
    let name = name.to_str()?;
    let key = name.get(..64)?;
    if !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let rest = &name[64..];
    if rest == ".wcv" {
        return Some(false);
    }
    let unique = rest.strip_prefix('.')?.strip_suffix(".tmp")?;
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let owned = match unique.split_once('-') {
        Some((pid, nanos)) => numeric(pid) && numeric(nanos) && !nanos.contains('-'),
        None => numeric(unique),
    };
    owned.then_some(true)
}

/// A value as bytes: [`MAGIC`], then the value, each node a tag and its payload.
pub fn encode(v: &Value) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    put_value(&mut out, v);
    out
}

/// The value `bytes` hold, or none if they aren't one whole.
pub fn decode(bytes: &[u8]) -> Option<Value> {
    let rest = bytes.strip_prefix(MAGIC.as_slice())?;
    let mut r = Reader { bytes: rest, at: 0 };
    let v = r.value(0)?;
    (r.at == r.bytes.len()).then_some(v)
}

fn put_u32(out: &mut Vec<u8>, x: u32) {
    out.extend_from_slice(&x.to_le_bytes());
}

fn put_value(out: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Scalar(c) => {
            out.push(0);
            put_const(out, c);
        }
        Value::Parts(parts) => {
            out.push(1);
            put_u32(out, parts.len() as u32);
            for p in parts {
                put_value(out, p);
            }
        }
        Value::Variant(tag, payload) => {
            out.push(2);
            put_u32(out, *tag);
            match payload {
                Some(p) => {
                    out.push(1);
                    put_value(out, p);
                }
                None => out.push(0),
            }
        }
        Value::Points(values) => {
            out.push(3);
            put_u32(out, values.len() as u32);
            for p in values {
                put_value(out, p);
            }
        }
        Value::Bytes(b) => {
            out.push(4);
            put_u32(out, b.len() as u32);
            out.extend_from_slice(b);
        }
    }
}

fn scalar_tag(s: ir::Scalar) -> u8 {
    use ir::Scalar as S;
    match s {
        S::Bool => 0,
        S::I8 => 1,
        S::U8 => 2,
        S::I16 => 3,
        S::U16 => 4,
        S::I32 => 5,
        S::U32 => 6,
        S::I64 => 7,
        S::U64 => 8,
        S::F32 => 9,
        S::F64 => 10,
    }
}

fn scalar_of(t: u8) -> Option<ir::Scalar> {
    use ir::Scalar as S;
    Some(match t {
        0 => S::Bool,
        1 => S::I8,
        2 => S::U8,
        3 => S::I16,
        4 => S::U16,
        5 => S::I32,
        6 => S::U32,
        7 => S::I64,
        8 => S::U64,
        9 => S::F32,
        10 => S::F64,
        _ => return None,
    })
}

fn put_const(out: &mut Vec<u8>, c: &ir::Const) {
    match *c {
        ir::Const::Bool(b) => out.extend_from_slice(&[0, u8::from(b)]),
        ir::Const::I32(x) => {
            out.push(1);
            out.extend_from_slice(&x.to_le_bytes());
        }
        ir::Const::U32(x) => {
            out.push(2);
            out.extend_from_slice(&x.to_le_bytes());
        }
        ir::Const::I64(x) => {
            out.push(3);
            out.extend_from_slice(&x.to_le_bytes());
        }
        ir::Const::U64(x) => {
            out.push(4);
            out.extend_from_slice(&x.to_le_bytes());
        }
        ir::Const::Small(s, x) => {
            out.push(5);
            out.push(scalar_tag(s));
            out.extend_from_slice(&x.to_le_bytes());
        }
        // Their bits, so a NaN's (canonical, §11) and a negative zero come back as they were.
        ir::Const::F32(x) => {
            out.push(6);
            out.extend_from_slice(&x.to_bits().to_le_bytes());
        }
        ir::Const::F64(x) => {
            out.push(7);
            out.extend_from_slice(&x.to_bits().to_le_bytes());
        }
    }
}

struct Reader<'b> {
    bytes: &'b [u8],
    at: usize,
}

/// How deep a value nests at most: deeper is a corrupt file (types nest at most 128 deep, §2).
const MAX_DEPTH: u32 = 4096;

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn values(&mut self, depth: u32) -> Option<Vec<Value>> {
        let n = self.u32()? as usize;
        // Each value takes at least two bytes: a length past what's left is corrupt.
        if n > self.bytes.len() - self.at {
            return None;
        }
        (0..n).map(|_| self.value(depth + 1)).collect()
    }

    fn value(&mut self, depth: u32) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        Some(match self.u8()? {
            0 => Value::Scalar(self.constant()?),
            1 => Value::Parts(self.values(depth)?),
            2 => {
                let tag = self.u32()?;
                let payload = match self.u8()? {
                    0 => None,
                    1 => Some(Box::new(self.value(depth + 1)?)),
                    _ => return None,
                };
                Value::Variant(tag, payload)
            }
            3 => Value::Points(self.values(depth)?),
            4 => {
                let n = self.u32()? as usize;
                Value::Bytes(self.take(n)?.to_vec())
            }
            _ => return None,
        })
    }

    fn constant(&mut self) -> Option<ir::Const> {
        Some(match self.u8()? {
            0 => ir::Const::Bool(self.u8()? != 0),
            1 => ir::Const::I32(self.u32()? as i32),
            2 => ir::Const::U32(self.u32()?),
            3 => ir::Const::I64(self.u64()? as i64),
            4 => ir::Const::U64(self.u64()?),
            5 => {
                let s = scalar_of(self.u8()?)?;
                ir::Const::Small(s, self.u64()? as i64)
            }
            6 => ir::Const::F32(f32::from_bits(self.u32()?)),
            7 => ir::Const::F64(f64::from_bits(self.u64()?)),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_reads_back_as_it_was_stored() {
        let v = Value::Parts(vec![
            Value::Scalar(ir::Const::F32(-0.0)),
            Value::Scalar(ir::Const::F64(f64::from_bits(0x7ff8000000000000))),
            Value::Scalar(ir::Const::Small(ir::Scalar::I8, -3)),
            Value::Variant(2, Some(Box::new(Value::Bytes(b"tile".to_vec())))),
            Value::Variant(0, None),
            Value::Points(vec![Value::Scalar(ir::Const::U64(u64::MAX)); 3]),
            Value::Scalar(ir::Const::Bool(true)),
        ]);
        let bytes = encode(&v);
        let back = decode(&bytes).expect("it reads back");
        // Compared by their bytes, as -0.0 == 0.0 and NaN != NaN.
        assert_eq!(encode(&back), bytes);
        // Cut short, or with a byte more, it's no value.
        assert!(decode(&bytes[..bytes.len() - 1]).is_none());
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(decode(&longer).is_none());
    }

    #[test]
    fn the_cache_is_the_shared_directory_else_the_packages() {
        let root = std::env::temp_dir();
        let canonical = root.canonicalize().expect("temporary directory");
        assert_eq!(dir_in(&root, None), Some(canonical.join("build/consts")));
        assert_eq!(dir_in(&root, Some("".into())), Some(canonical.join("build/consts")));
        assert_eq!(dir_in(&root, Some("/s/c".into())), Some(PathBuf::from("/s/c")));
        assert_eq!(dir_in(Path::new("/no/such/package"), None), None);
    }

    #[test]
    fn a_stored_value_is_found_by_its_key_and_another_key_misses() {
        let dir = std::env::temp_dir().join(format!("wrela-consts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let k = key(b"\0asm module", 1 << 34);
        assert_eq!(k, "9055858743647bc9bcd49a719c4b94f48e6d263cb84cbc0ded2af72ebfd714b8");
        assert_ne!(k, key(b"\0asm module", 1 << 35), "the fuel is part of the key");
        assert_ne!(k, key(b"\0asm modulf", 1 << 34));
        let v = Value::Bytes(vec![1, 2, 3]);
        assert!(get(&dir, &k).is_none());
        put(&dir, &k, &v);
        assert_eq!(get(&dir, &k), Some(v));
        clean(&dir);
        assert!(get(&dir, &k).is_some(), "a value used today is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn perf_cleanup_keeps_used_and_future_entries_and_discards_stale_files() {
        use std::time::{Duration, SystemTime};
        let dir =
            std::env::temp_dir().join(format!("wrela-consts-perf-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let now = SystemTime::now();
        let hour = Duration::from_secs(3600);
        let recent = format!("{}.wcv", "2".repeat(64));
        let current = format!("{}.123.tmp", "4".repeat(64));
        let future = format!("{}.123-456.tmp", "5".repeat(64));
        for (name, modified) in [
            (format!("{}.wcv", "1".repeat(64)), now - KEEP_FOR - hour),
            (recent.clone(), now - hour * 2),
            (format!("{}.123-456.tmp", "3".repeat(64)), now - hour * 2),
            (current.clone(), now),
            (future.clone(), now + hour),
        ] {
            std::fs::write(dir.join(&name), "cached").expect("write");
            std::fs::File::options()
                .write(true)
                .open(dir.join(&name))
                .expect("open")
                .set_modified(modified)
                .expect("set time");
        }
        clean(&dir);
        let mut kept: Vec<_> = std::fs::read_dir(&dir)
            .expect("read")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        kept.sort();
        assert_eq!(kept, [recent, current, future]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cleaning_removes_only_old_cache_owned_files() {
        let pkg = std::env::temp_dir().join(format!("wrela-consts-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&pkg);
        std::fs::create_dir_all(&pkg).expect("package");
        let dir = dir_in(&pkg, None).expect("cache path");
        std::fs::create_dir_all(&dir).expect("cache directory");
        let key = "1".repeat(64);
        let entry = dir.join(format!("{key}.wcv"));
        let temporary = dir.join(format!("{key}.123-456.tmp"));
        let old_temporary = dir.join(format!("{key}.123.tmp"));
        let other = dir.join("important.txt");
        let old = std::time::SystemTime::now()
            .checked_sub(KEEP_FOR + std::time::Duration::from_secs(3600))
            .expect("an old time");
        for path in [&entry, &temporary, &old_temporary, &other] {
            let file = std::fs::File::create(path).expect("file");
            file.set_modified(old).expect("old");
        }
        clean(&dir);
        assert!(!entry.exists(), "an old cache entry is removed");
        assert!(!temporary.exists(), "an old cache temporary is removed");
        assert!(!old_temporary.exists(), "an old-format cache temporary is removed");
        assert!(other.exists(), "a file the cache doesn't own is left alone");
        let _ = std::fs::remove_dir_all(pkg);
    }

    #[cfg(unix)]
    #[test]
    fn a_cache_cannot_escape_through_a_symbolic_link() {
        use std::os::unix::fs::symlink;

        let base = std::env::temp_dir().join(format!("wrela-consts-link-{}", std::process::id()));
        let pkg = base.join("package");
        let outside = base.join("outside");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(pkg.join("build")).expect("package build");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(outside.join("important.txt"), "keep").expect("outside file");
        symlink(&outside, pkg.join("build/consts")).expect("symlink");

        assert!(dir_in(&pkg, None).is_none());
        assert!(dir_in(&pkg, Some(pkg.join("build/consts").into_os_string())).is_none());
        clean(&pkg.join("build/consts"));
        assert_eq!(
            std::fs::read_to_string(outside.join("important.txt")).expect("outside file remains"),
            "keep"
        );
        let _ = std::fs::remove_dir_all(base);
    }
}
