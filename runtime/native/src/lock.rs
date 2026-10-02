//! The GPU lock: one GPU user at a time across processes.
//!
//! On 2026-10-01, a dozen concurrent headless Chrome runs starved macOS's WindowServer of GPU
//! time until its watchdog killed it. `tools/headless.py` therefore serializes GPU users with
//! `flock(2)` on `$TMPDIR/wrela-gpu.lock`, and the native host takes the same lock, so a Chrome
//! run and a native run queue instead of overlapping. The kernel releases an `flock` when its
//! holder exits, so a crashed run never leaves a stale lock.
//!
//! Within one process the lock is shared: every [`GpuLock`] alive in the process holds the same
//! `flock`, which is released when the last one drops. (Two `flock`s on separate opens of the
//! file would conflict even inside one process, so a thread holding a [`crate::Host`] would
//! deadlock against itself when creating a second.)

use crate::error::{Error, Result};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

/// How long to wait before giving up, as tools/headless.py does.
const WAIT_LIMIT: Duration = Duration::from_secs(3600);
const POLL: Duration = Duration::from_millis(500);

/// The lock file, shared with tools/headless.py (Python's `tempfile.gettempdir()`, which is
/// `$TMPDIR` on macOS, like [`std::env::temp_dir`]).
pub fn lock_path() -> PathBuf {
    std::env::temp_dir().join("wrela-gpu.lock")
}

/// The process's hold on the lock; released (by closing the file) when the last [`GpuLock`]
/// drops.
#[derive(Debug)]
struct Held {
    _file: File,
}

static SHARED: Mutex<Weak<Held>> = Mutex::new(Weak::new());

/// A share of this process's hold on the GPU lock.
#[derive(Clone, Debug)]
pub struct GpuLock {
    _held: Arc<Held>,
}

impl GpuLock {
    /// Takes the lock, waiting (up to an hour) while another process holds it. `who` is written
    /// into the lock file so a waiter can say what it's waiting for.
    pub fn acquire(who: &str) -> Result<GpuLock> {
        let mut shared = SHARED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(held) = shared.upgrade() {
            return Ok(GpuLock { _held: held });
        }
        let held = Arc::new(Held { _file: take(who)? });
        *shared = Arc::downgrade(&held);
        Ok(GpuLock { _held: held })
    }
}

fn open() -> Result<File> {
    let path = lock_path();
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never follow a symlink planted at the lock's path. std opens with O_CLOEXEC already.
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(&path).map_err(|e| Error::Lock(format!("can't open {}: {e}", path.display())))
}

fn take(who: &str) -> Result<File> {
    let mut file = open()?;
    let start = Instant::now();
    let mut announced = false;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) => {
                if !announced {
                    let mut holder = String::new();
                    let _ = (&file).read_to_string(&mut holder);
                    let holder = holder.trim();
                    let holder = if holder.is_empty() { "unknown" } else { holder };
                    eprintln!("waiting for the GPU lock, held by: {holder}");
                    announced = true;
                }
                if start.elapsed() >= WAIT_LIMIT {
                    return Err(Error::Lock(format!(
                        "gave up waiting for {} after an hour",
                        lock_path().display()
                    )));
                }
                std::thread::sleep(POLL);
            }
            Err(TryLockError::Error(e)) => {
                return Err(Error::Lock(format!("can't lock {}: {e}", lock_path().display())));
            }
        }
    }
    if announced {
        eprintln!("got the GPU lock after {:.0}s", start.elapsed().as_secs_f64());
    }
    // Say who holds it, for waiters' messages. Best effort: the lock is what matters.
    let note = format!("pid {}, {who}\n", std::process::id());
    let _ = file
        .set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)))
        .and_then(|_| file.write_all(note.as_bytes()));
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_one_hold_per_process() {
        let a = GpuLock::acquire("lock test").expect("lock");
        // A second acquisition in the same process mustn't wait on the first.
        let b = GpuLock::acquire("lock test").expect("lock");
        assert!(Arc::ptr_eq(&a._held, &b._held));
        drop(a);
        drop(b);
        assert!(SHARED.lock().expect("not poisoned").upgrade().is_none());
    }
}
