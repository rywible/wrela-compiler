//! A server's own services (language.md §19.4): what a server build's rooms ask their host for
//! beyond the network. One [`Services`] serves every room of a server process.
//!
//! - **The store** (`std::store`): keys and values, both bytes, kept in order of their keys in one
//!   SQLite file, which every room and every server build shares, so it outlives a build (a
//!   room's journal doesn't). A read (`store/get`, `store/scan`) is a request, answered from what
//!   is committed when the host takes it. A write (`store/write`) is a command: the host commits
//!   a call's writes in one transaction after the call returns, before its records are kept and
//!   before anything it sent goes ([`crate::rooms`]), so a peer never sees an answer whose write
//!   isn't kept.
//! - **Randomness** (`crypto/random`): bytes from the operating system's generator.
//! - **The time of day** (`time/unix`): the system clock's milliseconds since the Unix epoch,
//!   eight bytes.
//! - **Signatures** (`crypto/verify`): whether a signature is a key's over a message, by the
//!   algorithms passkeys use: ES256 (ECDSA on P-256 with SHA-256), Ed25519 and RS256 (RSA
//!   PKCS #1 v1.5 with SHA-256). The checks are the RustCrypto and dalek crates'; nothing here
//!   does arithmetic of its own.
//!
//! The services' bytes are std's: each request's and answer's layout is in its std function's
//! comment (`std::store`, `std::crypto`), and every number in them is a little-endian `u32`.

use rusqlite::OptionalExtension;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// The longest key.
pub const MAX_KEY: usize = 1024;
/// The longest value: 1020 KiB, so a put of the longest key and value fits in one host request.
pub const MAX_VALUE: usize = 1020 << 10;
/// The most bytes of keys and values a scan answers with: past them it stops early.
pub const MAX_SCAN: usize = 1 << 20;
/// The most randomness one request asks for.
pub const MAX_RANDOM: usize = 1024;

/// What a write does to one key.
const PUT: u8 = 1;
const DELETE: u8 = 2;

/// The signature algorithms `crypto/verify` knows, by std's numbers.
const ES256: u8 = 1;
const ED25519: u8 = 2;
const RS256: u8 = 3;

/// A server's services: its store, open for every room.
pub struct Services {
    store: Mutex<rusqlite::Connection>,
}

impl Services {
    /// The services with the store in `path` (made if it isn't there).
    pub fn open(path: &Path) -> Result<Services, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let db =
            rusqlite::Connection::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        // Write-ahead logging, flushed at each commit: a write a call made is on the disk before
        // the call's sends go.
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             CREATE TABLE IF NOT EXISTS kv (key BLOB PRIMARY KEY, value BLOB NOT NULL) WITHOUT ROWID;",
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Services { store: Mutex::new(db) })
    }

    /// The store's connection, for one request or one transaction.
    fn db(&self) -> Result<MutexGuard<'_, rusqlite::Connection>, String> {
        self.store.lock().map_err(|_| "the store's lock is poisoned".to_string())
    }

    /// The answer to request `service` with `body`: its bytes, or why there are none.
    pub fn answer(&self, service: &str, body: &[u8]) -> Result<Vec<u8>, String> {
        match service {
            "store/get" => self.get(body),
            "store/scan" => self.scan(body),
            "crypto/random" => random(body),
            "time/unix" => unix_ms(body).map(|ms| ms.to_le_bytes().to_vec()),
            "crypto/verify" => verify(body).map(|ok| vec![u8::from(ok)]),
            _ => Err(format!("this server has no service `{service}`")),
        }
    }

    /// Commits writes (`store/write` commands' bodies) in one transaction: all of them, or none.
    pub fn write(&self, batches: &[impl AsRef<[u8]>]) -> Result<(), String> {
        let mut ops = Vec::new();
        for b in batches {
            ops.extend(writes(b.as_ref())?);
        }
        let mut db = self.db()?;
        let tx = db.transaction().map_err(|e| e.to_string())?;
        for (key, value) in ops {
            match value {
                Some(v) => tx
                    .prepare_cached("INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)")
                    .and_then(|mut put| put.execute((key, v))),
                None => tx
                    .prepare_cached("DELETE FROM kv WHERE key = ?1")
                    .and_then(|mut delete| delete.execute((key,))),
            }
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// `store/get`: the key → 1 then the value, or 0 if it has none.
    fn get(&self, body: &[u8]) -> Result<Vec<u8>, String> {
        let key = key(body)?;
        self.db()?
            .query_row("SELECT value FROM kv WHERE key = ?1", (key,), |row| {
                let value = blob(row, 0)?;
                let mut out = Vec::with_capacity(value.len() + 1);
                out.push(1);
                out.extend_from_slice(value);
                Ok(out)
            })
            .optional()
            .map(|out| out.unwrap_or_else(|| vec![0]))
            .map_err(|e| e.to_string())
    }

    /// `store/scan`: from (a key), to (a key; empty: no end), at most `limit` entries, and 1 to go
    /// down from `to` rather than up from `from` → each entry with `from <= key < to`, in order,
    /// as its key and its value (each a length, then bytes), after their count. It stops early
    /// past [`MAX_SCAN`] bytes; the next scan starts after the last key it gave.
    fn scan(&self, body: &[u8]) -> Result<Vec<u8>, String> {
        let mut r = Bytes(body);
        let from = r.chunk()?;
        let to = r.chunk()?;
        let limit = r.word()?;
        let down = r.byte()? == 1;
        r.end()?;
        if from.len() > MAX_KEY || to.len() > MAX_KEY {
            return Err(format!("a key is longer than {MAX_KEY} bytes"));
        }
        let sql = match (to.is_empty(), down) {
            (true, false) => "SELECT key, value FROM kv WHERE key >= ?1 ORDER BY key ASC LIMIT ?2",
            (true, true) => "SELECT key, value FROM kv WHERE key >= ?1 ORDER BY key DESC LIMIT ?2",
            (false, false) => {
                "SELECT key, value FROM kv WHERE key >= ?1 AND key < ?3 ORDER BY key ASC LIMIT ?2"
            }
            (false, true) => {
                "SELECT key, value FROM kv WHERE key >= ?1 AND key < ?3 ORDER BY key DESC LIMIT ?2"
            }
        };
        let db = self.db()?;
        let mut stmt = db.prepare_cached(sql).map_err(|e| e.to_string())?;
        let mut rows =
            if to.is_empty() { stmt.query((from, limit)) } else { stmt.query((from, limit, to)) }
                .map_err(|e| e.to_string())?;
        let (mut out, mut count, mut bytes) = (vec![0u8; 4], 0u32, 0usize);
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let k = blob(row, 0).map_err(|e| e.to_string())?;
            let v = blob(row, 1).map_err(|e| e.to_string())?;
            bytes += k.len() + v.len();
            if count > 0 && bytes > MAX_SCAN {
                break;
            }
            for part in [k, v] {
                out.extend_from_slice(&(part.len() as u32).to_le_bytes());
                out.extend_from_slice(part);
            }
            count += 1;
        }
        out[..4].copy_from_slice(&count.to_le_bytes());
        Ok(out)
    }
}

/// A row's bytes, borrowed until the next row, with the same type error as `Row::get`.
fn blob<'a>(row: &'a rusqlite::Row<'_>, index: usize) -> rusqlite::Result<&'a [u8]> {
    match row.get_ref(index)? {
        rusqlite::types::ValueRef::Blob(bytes) => Ok(bytes),
        value => Err(rusqlite::Error::InvalidColumnType(
            index,
            row.as_ref().column_name(index)?.into(),
            value.data_type(),
        )),
    }
}

/// A write to one key: its new value, or none to delete it.
type Write<'a> = (&'a [u8], Option<&'a [u8]>);

/// A write's body: entries, each a kind (1 put, 2 delete), a key, and for a put its value. An
/// error is the program's: the body is malformed, or a key or a value is too long.
pub(crate) fn writes(body: &[u8]) -> Result<Vec<Write<'_>>, String> {
    let mut r = Bytes(body);
    let mut ops = Vec::new();
    while !r.0.is_empty() {
        let kind = r.byte()?;
        let k = r.chunk()?;
        if k.len() > MAX_KEY {
            return Err(format!("a key is longer than {MAX_KEY} bytes"));
        }
        let value = match kind {
            PUT => {
                let v = r.chunk()?;
                if v.len() > MAX_VALUE {
                    return Err(format!("a value is longer than {MAX_VALUE} bytes"));
                }
                Some(v)
            }
            DELETE => None,
            _ => return Err(format!("a store write of kind {kind}")),
        };
        ops.push((k, value));
    }
    Ok(ops)
}

/// A key alone, as `store/get` takes it.
fn key(body: &[u8]) -> Result<&[u8], String> {
    if body.len() > MAX_KEY {
        return Err(format!("a key is longer than {MAX_KEY} bytes"));
    }
    Ok(body)
}

/// `time/unix`: nothing → the system clock's milliseconds since the Unix epoch.
fn unix_ms(body: &[u8]) -> Result<u64, String> {
    Bytes(body).end()?;
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "the system clock is before 1970".to_string())?;
    Ok(since.as_millis() as u64)
}

/// `crypto/random`: how many bytes → that many from the operating system's generator.
fn random(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut r = Bytes(body);
    let n = r.word()? as usize;
    r.end()?;
    if n > MAX_RANDOM {
        return Err(format!("more than {MAX_RANDOM} random bytes at once"));
    }
    let mut out = vec![0u8; n];
    getrandom::fill(&mut out).map_err(|e| format!("the system's randomness: {e}"))?;
    Ok(out)
}

/// `crypto/verify`: an algorithm, a public key, a message and a signature → whether the
/// signature is the key's over the message. A key that isn't one of the algorithm's fails the
/// request; a signature that's malformed is simply not the key's.
///
/// - ES256: the key is a SEC1 point (65 bytes, uncompressed), the signature DER-encoded, as
///   WebAuthn gives it.
/// - Ed25519: the key is 32 bytes and the signature 64.
/// - RS256: the key is its modulus then its exponent (each a length, then big-endian bytes).
fn verify(body: &[u8]) -> Result<bool, String> {
    let mut r = Bytes(body);
    let alg = r.byte()?;
    let key = r.chunk()?;
    let message = r.chunk()?;
    let signature = r.chunk()?;
    r.end()?;
    match alg {
        ES256 => {
            use p256::ecdsa::signature::Verifier;
            let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(key)
                .map_err(|_| "not a P-256 public key".to_string())?;
            let Ok(sig) = p256::ecdsa::DerSignature::from_bytes(signature) else {
                return Ok(false);
            };
            Ok(key.verify(message, &sig).is_ok())
        }
        ED25519 => {
            let key: [u8; 32] =
                key.try_into().map_err(|_| "an Ed25519 key is 32 bytes".to_string())?;
            let key = ed25519_dalek::VerifyingKey::from_bytes(&key)
                .map_err(|_| "not an Ed25519 public key".to_string())?;
            let Ok(sig) = ed25519_dalek::Signature::from_slice(signature) else { return Ok(false) };
            Ok(key.verify_strict(message, &sig).is_ok())
        }
        RS256 => {
            use rsa::signature::Verifier;
            let mut k = Bytes(key);
            let n = rsa::BigUint::from_bytes_be(k.chunk()?);
            let e = rsa::BigUint::from_bytes_be(k.chunk()?);
            k.end()?;
            let key = rsa::RsaPublicKey::new(n, e).map_err(|e| format!("not an RSA key: {e}"))?;
            let key = rsa::pkcs1v15::VerifyingKey::<sha2::Sha256>::new(key);
            let Ok(sig) = rsa::pkcs1v15::Signature::try_from(signature) else { return Ok(false) };
            Ok(key.verify(message, &sig).is_ok())
        }
        _ => Err(format!("no signature algorithm {alg}")),
    }
}

/// A service's bytes, read from the front.
struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn byte(&mut self) -> Result<u8, String> {
        let (&b, rest) = self.0.split_first().ok_or("a service's bytes end early")?;
        self.0 = rest;
        Ok(b)
    }

    fn word(&mut self) -> Result<u32, String> {
        if self.0.len() < 4 {
            return Err("a service's bytes end early".into());
        }
        let (w, rest) = self.0.split_at(4);
        self.0 = rest;
        Ok(u32::from_le_bytes(w.try_into().expect("four bytes")))
    }

    /// A length, then that many bytes.
    fn chunk(&mut self) -> Result<&'a [u8], String> {
        let n = self.word()? as usize;
        if self.0.len() < n {
            return Err("a service's bytes end early".into());
        }
        let (c, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(c)
    }

    fn end(&self) -> Result<(), String> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err("a service's bytes go on past their end".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(b: &[u8]) -> Vec<u8> {
        [&(b.len() as u32).to_le_bytes()[..], b].concat()
    }

    fn put(k: &[u8], v: &[u8]) -> Vec<u8> {
        [&[PUT][..], &chunk(k), &chunk(v)].concat()
    }

    fn scan(
        s: &Services,
        from: &[u8],
        to: &[u8],
        limit: u32,
        down: bool,
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        let body = [&chunk(from)[..], &chunk(to), &limit.to_le_bytes(), &[u8::from(down)]].concat();
        let answer = s.answer("store/scan", &body).expect("a scan");
        let mut r = Bytes(&answer);
        let n = r.word().expect("a count");
        let mut out = Vec::new();
        for _ in 0..n {
            out.push((r.chunk().expect("key").to_vec(), r.chunk().expect("value").to_vec()));
        }
        r.end().expect("nothing after the entries");
        out
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wrela-services-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn the_store_keeps_writes_in_key_order_across_opening_it_again() {
        let d = dir("order");
        let path = d.join("store.sqlite");
        {
            let s = Services::open(&path).expect("open");
            let batch =
                [put(b"b/2", b"two"), put(b"b/1", b"one"), put(b"a", b"x"), put(b"c", b"y")]
                    .concat();
            s.write(&[batch]).expect("write");
            s.write(&[[&[DELETE][..], &chunk(b"c")].concat()]).expect("delete");
        }
        let s = Services::open(&path).expect("open again");
        assert_eq!(s.answer("store/get", b"b/1").expect("get"), [&[1u8][..], b"one"].concat());
        assert_eq!(s.answer("store/get", b"c").expect("get"), vec![0], "deleted");
        let up = scan(&s, b"b/", b"b0", 10, false);
        assert_eq!(
            up,
            vec![(b"b/1".to_vec(), b"one".to_vec()), (b"b/2".to_vec(), b"two".to_vec())]
        );
        let down = scan(&s, b"", b"", 2, true);
        assert_eq!(down.iter().map(|e| e.0.as_slice()).collect::<Vec<_>>(), [&b"b/2"[..], b"b/1"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_malformed_write_commits_nothing_of_its_transaction() {
        let d = dir("atomic");
        let s = Services::open(&d.join("store.sqlite")).expect("open");
        let good = put(b"k", b"v");
        let bad = vec![9, 0, 0, 0, 0];
        assert!(s.write(&[good, bad]).is_err());
        assert_eq!(s.answer("store/get", b"k").expect("get"), vec![0]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn borrowed_write_batches_keep_operation_order_and_empty_values() {
        let d = dir("borrowed");
        let s = Services::open(&d.join("store.sqlite")).expect("open");
        let first = [put(b"key", b"first"), put(b"", b"")].concat();
        let second =
            [[&[DELETE][..], &chunk(b"key")].concat(), put(b"key", b"last"), put(b"empty", b"")]
                .concat();
        s.write(&[first.as_slice(), second.as_slice()]).expect("borrowed batches");
        assert_eq!(s.answer("store/get", b"key").expect("get"), b"\x01last");
        assert_eq!(s.answer("store/get", b"").expect("empty key"), [1]);
        assert_eq!(s.answer("store/get", b"empty").expect("empty value"), [1]);
        assert_eq!(
            scan(&s, b"", b"", 10, false),
            vec![
                (Vec::new(), Vec::new()),
                (b"empty".to_vec(), Vec::new()),
                (b"key".to_vec(), b"last".to_vec()),
            ]
        );
        assert_eq!(
            scan(&s, b"empty", b"l", 10, true),
            vec![(b"key".to_vec(), b"last".to_vec()), (b"empty".to_vec(), Vec::new())]
        );
        assert!(scan(&s, b"", b"", 0, false).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_scan_keeps_its_first_entry_at_and_past_the_byte_limit() {
        let d = dir("scan-limit");
        let s = Services::open(&d.join("store.sqlite")).expect("open");
        // Values this long are past `MAX_VALUE`, so no write makes them; a store kept under a
        // larger limit may hold one, and reads still give it.
        let insert = |k: &[u8], v: &[u8]| {
            s.db()
                .expect("the store")
                .execute("INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)", (k, v))
                .expect("insert");
        };
        let mut value = vec![7u8; MAX_SCAN - 1];
        assert!(s.write(&[put(b"a", &value)]).is_err(), "longer than a write takes");
        insert(b"a", &value);
        s.write(&[put(b"b", b"")]).expect("write");
        assert_eq!(scan(&s, b"", b"", 10, false), vec![(b"a".to_vec(), value.clone())]);
        assert_eq!(scan(&s, b"b", b"", 10, false), vec![(b"b".to_vec(), Vec::new())]);
        value.push(8);
        insert(b"a", &value);
        assert_eq!(scan(&s, b"", b"", 10, false), vec![(b"a".to_vec(), value)]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn randomness_is_the_size_asked_and_bounded() {
        let a = random(&16u32.to_le_bytes()).expect("16 bytes");
        let b = random(&16u32.to_le_bytes()).expect("16 more");
        assert_eq!(a.len(), 16);
        assert_ne!(a, b, "two draws of 128 bits differ");
        assert!(random(&(MAX_RANDOM as u32 + 1).to_le_bytes()).is_err());
    }

    #[test]
    fn the_time_of_day_is_eight_bytes_of_milliseconds() {
        let d = dir("time");
        let s = Services::open(&d.join("store.sqlite")).expect("open");
        let answer = s.answer("time/unix", &[]).expect("the time");
        let ms = u64::from_le_bytes(answer.try_into().expect("eight bytes"));
        assert!(ms > 1_700_000_000_000, "after November 2023: {ms}");
        assert!(s.answer("time/unix", &[0]).is_err(), "it takes no bytes");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn verify_body(alg: u8, key: &[u8], message: &[u8], sig: &[u8]) -> Vec<u8> {
        [&[alg][..], &chunk(key), &chunk(message), &chunk(sig)].concat()
    }

    #[test]
    fn es256_checks_a_signature_and_refuses_another_message() {
        use p256::ecdsa::signature::Signer;
        let key = p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).expect("a key");
        let public = key.verifying_key().to_encoded_point(false);
        let sig: p256::ecdsa::Signature = key.sign(b"the message");
        let der = sig.to_der();
        assert!(
            verify(&verify_body(ES256, public.as_bytes(), b"the message", der.as_bytes()))
                .expect("checked")
        );
        assert!(
            !verify(&verify_body(ES256, public.as_bytes(), b"another", der.as_bytes()))
                .expect("checked")
        );
        assert!(
            !verify(&verify_body(ES256, public.as_bytes(), b"the message", b"junk"))
                .expect("checked")
        );
        assert!(verify(&verify_body(ES256, b"not a key", b"m", der.as_bytes())).is_err());
    }

    #[test]
    fn ed25519_checks_a_signature_and_refuses_another_message() {
        use ed25519_dalek::Signer;
        let key = ed25519_dalek::SigningKey::from_bytes(&[3u8; 32]);
        let sig = key.sign(b"hello");
        let public = key.verifying_key().to_bytes();
        assert!(
            verify(&verify_body(ED25519, &public, b"hello", &sig.to_bytes())).expect("checked")
        );
        assert!(
            !verify(&verify_body(ED25519, &public, b"hellO", &sig.to_bytes())).expect("checked")
        );
    }

    #[test]
    fn rs256_checks_a_signature_and_refuses_another_message() {
        use rsa::signature::{SignatureEncoding, Signer};
        use rsa::traits::PublicKeyParts;
        // A small key is enough to check the plumbing, and quick to make.
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(5);
        let private = rsa::RsaPrivateKey::new(&mut rng, 1024).expect("a key");
        let public = private.to_public_key();
        let signer = rsa::pkcs1v15::SigningKey::<sha2::Sha256>::new(private);
        let sig = signer.sign(b"payload").to_vec();
        let key =
            [&chunk(&public.n().to_bytes_be())[..], &chunk(&public.e().to_bytes_be())].concat();
        assert!(verify(&verify_body(RS256, &key, b"payload", &sig)).expect("checked"));
        assert!(!verify(&verify_body(RS256, &key, b"payloaD", &sig)).expect("checked"));
    }

    use rand_chacha::rand_core::SeedableRng;
}
