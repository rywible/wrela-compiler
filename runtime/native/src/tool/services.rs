//! The tool services (`std::tool`'s modules, language.md §15.5): each request's service and
//! bytes, read as std writes them, and the work that answers it. std owns both sides of every
//! layout; this is the host's half. Every answer starts with a byte, 0 for success then what the
//! service gives, or 1 then why it failed, in UTF-8. A service the host doesn't have fails the
//! request instead, as any host's does.

use super::blobs::Blobs;
use super::caps::{Caps, Entry, MOST_BYTES};
use std::sync::Mutex;
use std::time::Duration;

/// What a request is answered with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// The answer's bytes, starting with whether the service succeeded.
    Bytes(Vec<u8>),
    /// The host has no such service: the request fails (`REQUEST_FAILED`).
    Failed,
}

/// A success, giving `payload`.
fn ok(payload: &[u8]) -> Answer {
    let mut bytes = Vec::with_capacity(payload.len() + 1);
    bytes.push(0);
    bytes.extend_from_slice(payload);
    Answer::Bytes(bytes)
}

/// A failure, for `why`.
fn err(why: &str) -> Answer {
    let mut bytes = vec![1];
    bytes.extend_from_slice(why.as_bytes());
    Answer::Bytes(bytes)
}

fn answer(result: Result<Vec<u8>, String>) -> Answer {
    match result {
        Ok(payload) => ok(&payload),
        Err(why) => err(&why),
    }
}

/// What every service shares: the capability context and the blobs.
pub(crate) struct Services {
    pub caps: Caps,
    pub blobs: Mutex<Blobs>,
    /// How long a blob's write to a file waits before it writes: a test's slow disk.
    pub io_delay: Duration,
}

impl Services {
    fn blobs(&self) -> std::sync::MutexGuard<'_, Blobs> {
        // A worker that panicked while it held them leaves them as they were.
        self.blobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Work on a worker thread: given the services and the request's number.
pub(crate) type Task = Box<dyn FnOnce(&Services, u32) -> Answer + Send>;

/// How a request is carried out.
pub(crate) enum Work {
    /// Answered now, after the call that made it.
    Now(Answer),
    /// Answered, with no bytes, once this time has passed.
    Timer(Duration),
    /// Answered by a worker.
    Worker(Task),
}

/// A request's bytes, read in order as std's `Writer` wrote them.
struct Body<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Body<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.bytes.len());
        let Some(end) = end else { return Err("the request's bytes end too soon".into()) };
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("four bytes")))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().expect("eight bytes")))
    }

    fn str(&mut self) -> Result<String, String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "a path isn't UTF-8".to_string())
    }

    fn rest(&mut self) -> &'a [u8] {
        let out = &self.bytes[self.at..];
        self.at = self.bytes.len();
        out
    }

    fn end(&self) -> Result<(), String> {
        if self.at == self.bytes.len() { Ok(()) } else { Err("the request has bytes left".into()) }
    }
}

/// How request `service`, with `body`, is carried out.
pub(crate) fn route(service: &str, body: &[u8]) -> Work {
    let Some(name) = service.strip_prefix("tool/") else { return Work::Now(Answer::Failed) };
    let mut b = Body { bytes: body, at: 0 };
    match name {
        "time/after" => match b.u64().and_then(|bits| b.end().map(|_| f64::from_bits(bits))) {
            Ok(s) if (0.0..1e9).contains(&s) => Work::Timer(Duration::from_secs_f64(s)),
            Ok(s) => Work::Now(err(&format!("can't wait {s} seconds"))),
            Err(why) => Work::Now(err(&why)),
        },
        "time/utc" => {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
            let now = now.unwrap_or_default();
            let mut payload = now.as_secs().to_le_bytes().to_vec();
            payload.extend_from_slice(&now.subsec_nanos().to_le_bytes());
            Work::Now(ok(&payload))
        }
        "files/read" | "files/list" | "files/stat" | "files/remove" | "files/remove_all"
        | "files/make_dirs" | "files/read_blob" => {
            let path = match b.str().and_then(|p| b.end().map(|_| p)) {
                Ok(p) => p,
                Err(why) => return Work::Now(err(&why)),
            };
            let name = name.to_string();
            Work::Worker(Box::new(move |s, request| files(s, request, &name, &path)))
        }
        "files/write" => {
            let path = match b.str() {
                Ok(p) => p,
                Err(why) => return Work::Now(err(&why)),
            };
            let bytes = b.rest().to_vec();
            if bytes.len() > MOST_BYTES {
                return Work::Now(err("more than 512 KiB at once: write a blob"));
            }
            Work::Worker(Box::new(move |s, _| answer(s.caps.write(&path, &bytes).map(|_| vec![]))))
        }
        "files/write_blob" => {
            let parsed = b.str().and_then(|p| Ok((p, b.u32()?))).and_then(|v| b.end().map(|_| v));
            let (path, id) = match parsed {
                Ok(v) => v,
                Err(why) => return Work::Now(err(&why)),
            };
            Work::Worker(Box::new(move |s, _| {
                // The bytes are shared, not copied, and written once the table's lock is let go.
                let bytes = match s.blobs().get(id) {
                    Ok(b) => b,
                    Err(why) => return err(&why),
                };
                answer(
                    s.caps
                        .write_with(&path, |f| {
                            std::thread::sleep(s.io_delay);
                            f.write_all(&bytes)
                        })
                        .map(|_| vec![]),
                )
            }))
        }
        "blob/new" => match b.end() {
            Ok(()) => Work::Worker(Box::new(|s, request| {
                answer(s.blobs().create(request).map(|id| id.to_le_bytes().to_vec()))
            })),
            Err(why) => Work::Now(err(&why)),
        },
        "blob/append" => {
            let id = match b.u32() {
                Ok(id) => id,
                Err(why) => return Work::Now(err(&why)),
            };
            let bytes = b.rest().to_vec();
            if bytes.len() > MOST_BYTES {
                return Work::Now(err("more than 512 KiB at once"));
            }
            Work::Worker(Box::new(move |s, _| answer(s.blobs().append(id, &bytes).map(|_| vec![]))))
        }
        "blob/read" => {
            let parsed = (|| Ok::<_, String>((b.u32()?, b.u64()?, b.u32()?)))();
            match parsed.and_then(|v| b.end().map(|_| v)) {
                Ok((_, _, len)) if len as usize > MOST_BYTES => {
                    Work::Now(err("more than 512 KiB at once"))
                }
                Ok((id, offset, len)) => Work::Worker(Box::new(move |s, _| {
                    answer(s.blobs().read(id, offset, len as usize))
                })),
                Err(why) => Work::Now(err(&why)),
            }
        }
        "blob/len" | "blob/release" => {
            let id = match b.u32().and_then(|id| b.end().map(|_| id)) {
                Ok(id) => id,
                Err(why) => return Work::Now(err(&why)),
            };
            if name == "blob/len" {
                Work::Worker(Box::new(move |s, _| {
                    answer(s.blobs().get(id).map(|b| (b.len() as u64).to_le_bytes().to_vec()))
                }))
            } else {
                Work::Worker(Box::new(move |s, _| answer(s.blobs().release(id).map(|_| vec![]))))
            }
        }
        _ => Work::Now(Answer::Failed),
    }
}

/// An entry as `files::read_list` and `read_entry` read it: (the name), the kind, the size.
fn write_entry(out: &mut Vec<u8>, e: &Entry, named: bool) {
    if named {
        out.extend_from_slice(&(e.name.len() as u32).to_le_bytes());
        out.extend_from_slice(e.name.as_bytes());
    }
    out.push(e.kind as u8);
    out.extend_from_slice(&e.size.to_le_bytes());
}

/// The file services that take a path alone.
fn files(s: &Services, request: u32, name: &str, path: &str) -> Answer {
    let c = &s.caps;
    answer(match name {
        "files/read" => c.read(path),
        "files/list" => c.list(path).map(|entries| {
            let mut out = (entries.len() as u32).to_le_bytes().to_vec();
            for e in &entries {
                write_entry(&mut out, e, true);
            }
            out
        }),
        "files/stat" => c.stat(path).map(|e| {
            let mut out = Vec::new();
            write_entry(&mut out, &e, false);
            out
        }),
        "files/remove" => c.remove(path).map(|_| vec![]),
        "files/remove_all" => c.remove_all(path).map(|_| vec![]),
        "files/make_dirs" => c.make_dirs(path).map(|_| vec![]),
        "files/read_blob" => read_blob(s, request, path),
        _ => unreachable!("routed only the file services"),
    })
}

/// The file at `path` as a new blob, made for `request`: its handle. A file that doesn't fit the
/// budget is refused before it's read; one that fails part way releases its blob.
fn read_blob(s: &Services, request: u32, path: &str) -> Result<Vec<u8>, String> {
    let id = s.blobs().create(request)?;
    let read =
        s.caps.read_chunks(path, |size| s.blobs().fits(size), |chunk| s.blobs().append(id, chunk));
    match read {
        Ok(()) => Ok(id.to_le_bytes().to_vec()),
        Err(why) => {
            let _ = s.blobs().release(id);
            Err(why)
        }
    }
}
