//! Constants cached by their inputs, between builds (language.md §10). A computed constant is
//! lowered into a module of its own: the code it runs, the values of the constants it reads and
//! the files it embeds, nothing else. The module's WASM is the constant's key, with the fuel it
//! may use, so a constant is computed again only when one of those changes: an edit to a
//! function it doesn't call leaves its module, and its key, as they were.
//!
//! The cache is a directory of files in the package's `build` directory (`build/consts`), one
//! per key, each a constant's value ([`encode`]); and, when `WRELA_CONST_CACHE` names one, a
//! directory every package and checkout that set it share. A build reads the package's first,
//! then the shared one (a hit there is kept in the package's too), and writes both: so the
//! tests, which read only the package's, find what a session's builds computed or found. A file
//! is written whole to a temporary name and renamed into place, so two builds at once never read
//! half of one. Files no build has used for a week are removed.

use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use wrela_ir as ir;
use wrela_lower::Value;

/// The format's first bytes and version: a file of another version is a miss.
const MAGIC: &[u8; 8] = b"wrelac\x00\x03";

/// A key's encoded bytes, and the checksum at the end of an entry.
const KEY_BYTES: usize = 64;
const CHECKSUM_BYTES: usize = 32;

/// How long an entry no build has used is kept.
const KEEP_FOR: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// The cache's directories for the package at `root`, in the order a build reads them: its
/// `build/consts`, then the one `WRELA_CONST_CACHE` names, if it's set. One directory can serve
/// every package and checkout, as a key is all a value depends on: a fresh checkout reads the
/// constants another one computed.
pub fn dirs_of(root: &Path) -> Vec<PathBuf> {
    dirs_in(root, std::env::var_os("WRELA_CONST_CACHE"))
}

fn dirs_in(root: &Path, shared: Option<std::ffi::OsString>) -> Vec<PathBuf> {
    let own = root.canonicalize().ok().map(|r| r.join("build").join("consts"));
    let shared = shared.filter(|d| !d.is_empty()).map(PathBuf::from);
    own.into_iter().chain(shared).filter(|d| safe(d).is_some()).collect()
}

/// The value cached under `key` in the first of `dirs` that has it, kept in each before it.
pub fn get_in(dirs: &[PathBuf], key: &str) -> Option<Value> {
    for (i, dir) in dirs.iter().enumerate() {
        if let Some(v) = get(dir, key) {
            for earlier in &dirs[..i] {
                put(earlier, key, &v);
            }
            return Some(v);
        }
    }
    None
}

/// Stores `value` under `key` in each of `dirs`.
pub fn put_in(dirs: &[PathBuf], key: &str, value: &Value) {
    for dir in dirs {
        put(dir, key, value);
    }
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

/// A constant's key: its module's WASM and the fuel it may use, with the identity of the code
/// that runs it and reads its value back (build.rs: those sources' hash, and wasmtime's version).
/// How many threads computed it isn't part of it: results don't depend on them (§6.12).
pub fn key(wasm: &[u8], fuel: u64) -> String {
    let mut h = Sha256::new();
    h.update(MAGIC);
    h.update(env!("WRELA_CONST_IDENTITY").as_bytes());
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
    let value = decode(key, &bytes)?;
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
    let Some(bytes) = encode(key, value) else { return };
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

/// A value as bytes: [`MAGIC`], the key whose file may hold it, the value (each node a tag and
/// its payload), and a checksum of all of those bytes. Binding the contents to the key makes a
/// valid entry copied or renamed under another key a miss; the checksum makes a damaged entry
/// one too.
pub fn encode(key: &str, v: &Value) -> Option<Vec<u8>> {
    if key.len() != KEY_BYTES || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(key.as_bytes());
    put_value(&mut out, v);
    let checksum = Sha256::digest(&out);
    out.extend_from_slice(&checksum);
    Some(out)
}

/// The value `bytes` hold for `key`, or none if they aren't one whole, intact entry for it.
pub fn decode(key: &str, bytes: &[u8]) -> Option<Value> {
    if key.len() != KEY_BYTES || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let checksum_at = bytes.len().checked_sub(CHECKSUM_BYTES)?;
    let (body, stored_checksum) = bytes.split_at(checksum_at);
    let checksum: [u8; CHECKSUM_BYTES] = Sha256::digest(body).into();
    if checksum.as_slice() != stored_checksum {
        return None;
    }
    let rest = body.strip_prefix(MAGIC.as_slice())?.strip_prefix(key.as_bytes())?;
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
        let cache_key = key(b"\0asm codec", 7);
        let v = Value::Parts(vec![
            Value::Scalar(ir::Const::F32(-0.0)),
            Value::Scalar(ir::Const::F64(f64::from_bits(0x7ff8000000000000))),
            Value::Scalar(ir::Const::Small(ir::Scalar::I8, -3)),
            Value::Variant(2, Some(Box::new(Value::Bytes(b"tile".to_vec())))),
            Value::Variant(0, None),
            Value::Points(vec![Value::Scalar(ir::Const::U64(u64::MAX)); 3]),
            Value::Scalar(ir::Const::Bool(true)),
        ]);
        let bytes = encode(&cache_key, &v).expect("a valid key");
        let back = decode(&cache_key, &bytes).expect("it reads back");
        // Compared by their bytes, as -0.0 == 0.0 and NaN != NaN.
        assert_eq!(encode(&cache_key, &back).expect("a valid key"), bytes);
        // Cut short, or with a byte more, it's no value.
        assert!(decode(&cache_key, &bytes[..bytes.len() - 1]).is_none());
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(decode(&cache_key, &longer).is_none());
        assert!(decode(&key(b"\0asm another", 7), &bytes).is_none());
    }

    #[test]
    fn the_cache_is_the_packages_then_the_shared_directory() {
        let root = std::env::temp_dir();
        let own = root.canonicalize().expect("temporary directory").join("build/consts");
        assert_eq!(dirs_in(&root, None), std::slice::from_ref(&own));
        assert_eq!(dirs_in(&root, Some("".into())), std::slice::from_ref(&own));
        assert_eq!(dirs_in(&root, Some("/s/c".into())), [own, PathBuf::from("/s/c")]);
        assert_eq!(dirs_in(Path::new("/no/such/package"), None), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_value_found_in_the_shared_directory_is_kept_in_the_packages() {
        let base = std::env::temp_dir().join(format!("wrela-consts-two-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (own, shared) = (base.join("own"), base.join("shared"));
        let dirs = [own.clone(), shared.clone()];
        let (k, v) = (key(b"\0asm two", 1), Value::Bytes(vec![7]));
        put(&shared, &k, &v);
        assert!(get(&own, &k).is_none());
        assert_eq!(get_in(&dirs, &k), Some(v.clone()));
        assert_eq!(get(&own, &k), Some(v.clone()), "kept in the package's");
        let k2 = key(b"\0asm three", 1);
        put_in(&dirs, &k2, &v);
        assert!(get(&own, &k2).is_some() && get(&shared, &k2).is_some(), "written to both");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_stored_value_is_found_by_its_key_and_another_key_misses() {
        let dir = std::env::temp_dir().join(format!("wrela-consts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let k = key(b"\0asm module", 1 << 34);
        // The same every time, for one build of the compiler (its code's hash is in it).
        assert_eq!(k, key(b"\0asm module", 1 << 34));
        assert!(k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit()), "{k}");
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

    /// A valid entry belongs only to the key encoded in it, and any damaged byte makes it a
    /// miss. A miss is recomputed by the caller, so cache files cannot change program meaning.
    #[test]
    fn an_entry_is_bound_to_its_key_and_checksum() {
        let dir =
            std::env::temp_dir().join(format!("wrela-consts-integrity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (seven, nine) = (key(b"\0asm seven", 1), key(b"\0asm nine", 1));
        let value = Value::Scalar(ir::Const::U32(9));
        put(&dir, &nine, &value);

        let nine_file = dir.join(format!("{nine}.wcv"));
        let seven_file = dir.join(format!("{seven}.wcv"));
        std::fs::copy(&nine_file, &seven_file).expect("copy under another key");
        assert!(get(&dir, &seven).is_none(), "an entry copied under another key misses");

        let mut damaged = std::fs::read(&nine_file).expect("entry");
        let payload = MAGIC.len() + KEY_BYTES + 2;
        damaged[payload] ^= 1;
        std::fs::write(&nine_file, damaged).expect("damage entry");
        assert!(get(&dir, &nine).is_none(), "an entry whose payload changed misses");
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
        let dir = dirs_in(&pkg, None).into_iter().next().expect("cache path");
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

        assert!(dirs_in(&pkg, None).is_empty());
        assert!(dirs_in(&pkg, Some(pkg.join("build/consts").into_os_string())).is_empty());
        clean(&pkg.join("build/consts"));
        assert_eq!(
            std::fs::read_to_string(outside.join("important.txt")).expect("outside file remains"),
            "keep"
        );
        let _ = std::fs::remove_dir_all(base);
    }
}
