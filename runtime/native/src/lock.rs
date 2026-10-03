//! The GPU lock: one GPU user at a time, across processes and across a process's threads.
//!
//! On 2026-10-01, a dozen concurrent headless Chrome runs starved macOS's WindowServer of GPU
//! time until its watchdog killed it. `tools/headless.py` therefore serializes GPU users with
//! `flock(2)` on one lock file (see [`lock_path`]), and the native host takes the
//! same lock, so a Chrome run and a native run queue instead of overlapping. The kernel releases
//! an `flock` when its holder exits, so a crashed run never leaves a stale lock.
//!
//! Within one process, the threads queue too: one test binary runs its GPU tests on parallel
//! threads, and each would otherwise time its kernels on a GPU the others are using. The lock is
//! held by one thread at a time, and that thread can take it again (a test that loads two
//! [`crate::Host`]s, say): every [`GpuLock`] of the thread shares one `flock`, released when the
//! last one drops. (Two `flock`s on separate opens of the file would conflict even inside one
//! process, so a thread can't take a second one.) A [`GpuLock`] stays on the thread that took
//! it.

use crate::error::{Error, Result};
use std::ffi::OsString;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

/// How long to wait before giving up, as tools/headless.py does.
const WAIT_LIMIT: Duration = Duration::from_secs(3600);
const POLL: Duration = Duration::from_millis(500);

/// The lock file, shared with tools/headless.py, which picks it by the same rule: in `$TMPDIR`
/// (macOS sets it to a per-user directory) when that's set, else in the home directory. Never
/// in world-writable /tmp, where Python's `tempfile.gettempdir()` falls back to. So the runs
/// that queue are those that see the same `$TMPDIR` (normally all of one user's); a run with
/// another one, or another user's, takes a lock of its own.
fn lock_path() -> PathBuf {
    lock_path_in(std::env::var_os("TMPDIR"), std::env::home_dir())
}

fn lock_path_in(tmpdir: Option<OsString>, home: Option<PathBuf>) -> PathBuf {
    match tmpdir.filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("wrela-gpu.lock"),
        None => home.unwrap_or_else(|| PathBuf::from(".")).join(".wrela-gpu.lock"),
    }
}

/// The process's hold on the lock: the thread holding it, how many [`GpuLock`]s it has, and
/// the locked file (closed, which releases the `flock`, when the count drops to 0).
struct Hold {
    thread: Option<ThreadId>,
    count: usize,
    file: Option<File>,
}

static HOLD: Mutex<Hold> = Mutex::new(Hold { thread: None, count: 0, file: None });
/// Signalled when the hold is released.
static RELEASED: Condvar = Condvar::new();

fn hold() -> MutexGuard<'static, Hold> {
    HOLD.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A share of this thread's hold on the GPU lock. It can't move to another thread.
#[derive(Debug)]
pub struct GpuLock {
    _thread_bound: PhantomData<*const ()>,
}

impl Clone for GpuLock {
    fn clone(&self) -> GpuLock {
        hold().count += 1;
        GpuLock { _thread_bound: PhantomData }
    }
}

impl GpuLock {
    /// Takes the lock, waiting (up to an hour) while another thread or process holds it. `who`
    /// is written into the lock file so a waiter can say what it's waiting for.
    pub fn acquire(who: &str) -> Result<GpuLock> {
        let me = std::thread::current().id();
        let lock = GpuLock { _thread_bound: PhantomData };
        let mut h = hold();
        if h.thread == Some(me) {
            h.count += 1;
            return Ok(lock);
        }
        let start = Instant::now();
        let mut announced = false;
        while h.thread.is_some() {
            if !announced {
                eprintln!("waiting for the GPU lock, held by another thread of this process");
                announced = true;
            }
            let left = WAIT_LIMIT.saturating_sub(start.elapsed());
            if left.is_zero() {
                return Err(Error::Lock("gave up waiting for another thread after an hour".into()));
            }
            h = RELEASED.wait_timeout(h, left).unwrap_or_else(|p| p.into_inner()).0;
        }
        // This thread's now; the file is taken without holding the mutex, so the thread's other
        // users (none yet) and other threads' waits aren't blocked behind another process.
        h.thread = Some(me);
        h.count = 1;
        drop(h);
        match take(who) {
            Ok(file) => {
                hold().file = Some(file);
                Ok(lock)
            }
            Err(e) => {
                // `lock` hasn't been handed out; its drop gives the hold back.
                drop(lock);
                Err(e)
            }
        }
    }
}

impl Drop for GpuLock {
    fn drop(&mut self) {
        let mut h = hold();
        h.count -= 1;
        if h.count == 0 {
            h.thread = None;
            h.file = None;
            RELEASED.notify_all();
        }
    }
}

fn open(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never follow a symlink planted at the lock's path. std opens with O_CLOEXEC already.
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).map_err(|e| Error::Lock(format!("can't open {}: {e}", path.display())))
}

fn take(who: &str) -> Result<File> {
    let path = lock_path();
    let mut file = open(&path)?;
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
                        path.display()
                    )));
                }
                std::thread::sleep(POLL);
            }
            Err(TryLockError::Error(e)) => {
                return Err(Error::Lock(format!("can't lock {}: {e}", path.display())));
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
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn a_thread_shares_its_hold_and_others_wait() {
        let a = GpuLock::acquire("lock test").expect("lock");
        // A second acquisition on the same thread mustn't wait on the first.
        let b = GpuLock::acquire("lock test").expect("lock");
        let me = std::thread::current().id();
        let h = hold();
        assert_eq!((h.thread, h.count), (Some(me), 2));
        drop(h);
        // Another thread waits until both are gone.
        let got = AtomicBool::new(false);
        std::thread::scope(|s| {
            let waiter = s.spawn(|| {
                let _l = GpuLock::acquire("lock test, the other thread").expect("lock");
                got.store(true, Ordering::SeqCst);
            });
            std::thread::sleep(Duration::from_millis(200));
            assert!(!got.load(Ordering::SeqCst), "took the lock while this thread held it");
            drop(a);
            std::thread::sleep(Duration::from_millis(100));
            assert!(!got.load(Ordering::SeqCst), "took the lock while this thread held it");
            drop(b);
            waiter.join().expect("the other thread");
        });
        assert!(got.load(Ordering::SeqCst));
        let h = hold();
        assert!(h.thread.is_none() && h.count == 0 && h.file.is_none());
    }

    #[test]
    fn the_lock_is_in_tmpdir_else_home() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            lock_path_in(Some("/t/x".into()), home.clone()),
            PathBuf::from("/t/x/wrela-gpu.lock")
        );
        assert_eq!(
            lock_path_in(Some("".into()), home.clone()),
            PathBuf::from("/home/u/.wrela-gpu.lock")
        );
        assert_eq!(lock_path_in(None, home), PathBuf::from("/home/u/.wrela-gpu.lock"));
    }
}
