//! Constants cached by their inputs, between builds (language.md §10). A computed constant is
//! lowered into a module of its own: the code it runs, the values of the constants it reads and
//! the files it embeds, nothing else. The module's WASM is the constant's key, with the fuel it
//! may use, so a constant is computed again only when one of those changes: an edit to a
//! function it doesn't call leaves its module, and its key, as they were.
//!
//! The cache is a directory of files in the package's `build` directory (`build/consts`), one
//! per key, each a constant's value ([`encode`]). A file is written whole to a temporary name
//! and renamed into place, so two builds at once never read half of one. Files no build has used
//! for a week are removed.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use wrela_ir as ir;
use wrela_lower::Value;

/// The format's first bytes and version: a file of another version is a miss.
const MAGIC: &[u8; 8] = b"wrelac\x00\x02";

/// How long an entry no build has used is kept.
const KEEP_FOR: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// The cache's directory for the package at `root`: its `build/consts`.
pub fn dir_of(root: &Path) -> Option<PathBuf> {
    root.is_dir().then(|| root.join("build").join("consts"))
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
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The value cached under `key` in `dir`, if there's one that reads back whole. A hit marks the
/// file used now, so it's kept.
pub fn get(dir: &Path, key: &str) -> Option<Value> {
    let path = dir.join(format!("{key}.wcv"));
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
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let bytes = encode(value);
    let tmp = dir.join(format!("{key}.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, &bytes).is_ok()
        && std::fs::rename(&tmp, dir.join(format!("{key}.wcv"))).is_err()
    {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Removes the entries in `dir` that no build has used for [`KEEP_FOR`], and temporary files
/// left by a build that stopped.
pub fn clean(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = std::time::SystemTime::now();
    for e in entries.flatten() {
        let path = e.path();
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > KEEP_FOR);
        let stale_tmp = path.extension().is_some_and(|x| x == "tmp")
            && e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age.as_secs() > 3600);
        if old || stale_tmp {
            let _ = std::fs::remove_file(path);
        }
    }
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
    fn a_stored_value_is_found_by_its_key_and_another_key_misses() {
        let dir = std::env::temp_dir().join(format!("wrela-consts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let k = key(b"\0asm module", 1 << 34);
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
}
