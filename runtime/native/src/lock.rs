//! The GPU lock: who uses the GPU at once, across processes and across a process's threads.
//!
//! On 2026-10-01, a dozen concurrent headless Chrome runs starved macOS's WindowServer of GPU
//! time until its watchdog killed it. `tools/headless.py` therefore serializes GPU users with
//! `flock(2)` on one lock file (see [`lock_path`]), and the native host takes the
//! same lock, so a Chrome run and a native run queue instead of overlapping. The kernel releases
//! an `flock` when its holder exits, so a crashed run never leaves a stale lock.
//!
//! Within one process, the threads queue too: one test binary runs its GPU tests on parallel
//! threads, and each would otherwise time its kernels on a GPU the others are using. By default
//! the lock is held by one thread at a time, and that thread can take it again (a test that
//! loads two [`crate::Host`]s, say): every [`GpuLock`] of the thread shares one `flock`, released
//! when the last one drops. (Two `flock`s on separate opens of the file would conflict even
//! inside one process, so a thread can't take a second one.) A [`GpuLock`] stays on the thread
//! that took it.
//!
//! **Sharing.** Runs that check what the GPU computes, not how long it takes, can share it:
//! with `WRELA_GPU_SHARED=n` (n ≥ 2), up to n of a process's threads hold the lock at once, and
//! the process takes the `flock` shared, so other sharing processes hold it beside it (so does
//! `tools/headless.py` with the variable, which runs one headless Chrome at a time by a lock of
//! its own). A run that takes it alone (a process without the variable; a [`GpuLock::alone`])
//! still waits for every sharer, and they for it. `tools/check.sh` shares the GPU in its tests
//! and never in its timing runs.

use crate::error::{Error, Result};
use std::ffi::OsString;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
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

/// How many of this process's threads may hold the lock at once: `WRELA_GPU_SHARED`, else 1.
fn sharers() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| sharers_from(std::env::var("WRELA_GPU_SHARED").ok().as_deref()))
}

fn sharers_from(var: Option<&str>) -> usize {
    var.and_then(|v| v.trim().parse().ok()).unwrap_or(1).max(1)
}

/// A thread's hold: its thread, how many [`GpuLock`]s it has, and how many threads it took the
/// lock to share with (itself included).
struct Holder {
    thread: ThreadId,
    count: usize,
    sharers: usize,
}

/// The process's hold on the lock: the threads holding it, and the locked file (closed, which
/// releases the `flock`, when the last thread's hold goes). While one thread takes the file,
/// `taking` keeps the others that would join it waiting; while threads wait to hold it alone
/// (`alone_waiting`), no sharer joins, so sharers that come and go can't keep them waiting.
struct Hold {
    holders: Vec<Holder>,
    file: Option<File>,
    taking: bool,
    alone_waiting: usize,
}

impl Hold {
    /// Whether a thread that shares with `sharers` threads can join the holders now: there's
    /// room by its count and by every holder's, and no thread waits to hold it alone.
    fn room_for(&self, sharers: usize) -> bool {
        let limit = self.holders.iter().map(|h| h.sharers).fold(sharers, usize::min);
        let queued = sharers > 1 && self.alone_waiting > 0;
        !self.taking && !queued && self.holders.len() < limit
    }
}

static HOLD: Mutex<Hold> =
    Mutex::new(Hold { holders: Vec::new(), file: None, taking: false, alone_waiting: 0 });
/// Signalled when a hold is released or the file is taken.
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
        let me = std::thread::current().id();
        let mut h = hold();
        h.holders.iter_mut().find(|x| x.thread == me).expect("this thread's hold").count += 1;
        GpuLock { _thread_bound: PhantomData }
    }
}

impl GpuLock {
    /// Takes the lock, waiting (up to an hour) while it's held by other threads or processes it
    /// can't share it with (one at a time, unless `WRELA_GPU_SHARED` says otherwise). `who` is
    /// written into the lock file so a waiter can say what it's waiting for.
    pub fn acquire(who: &str) -> Result<GpuLock> {
        GpuLock::acquire_sharing(who, sharers())
    }

    /// Takes the lock alone, whatever `WRELA_GPU_SHARED` says: for a run that times the GPU.
    pub fn alone(who: &str) -> Result<GpuLock> {
        GpuLock::acquire_sharing(who, 1)
    }

    /// Takes the lock with up to `sharers` threads holding it, this one included; the `flock`
    /// is shared when `sharers` > 1. A thread that holds the lock already takes it again, as it
    /// holds it (so a timing run takes it alone before anything else on its thread takes it).
    fn acquire_sharing(who: &str, sharers: usize) -> Result<GpuLock> {
        let me = std::thread::current().id();
        // Made only once its hold is counted: its drop gives the hold back.
        let lock = || GpuLock { _thread_bound: PhantomData };
        let mut h = hold();
        if let Some(mine) = h.holders.iter_mut().find(|x| x.thread == me) {
            mine.count += 1;
            return Ok(lock());
        }
        let start = Instant::now();
        let mut announced = false;
        let alone = sharers == 1;
        h.alone_waiting += usize::from(alone);
        while !h.room_for(sharers) {
            if !announced {
                eprintln!("waiting for the GPU lock, held by other threads of this process");
                announced = true;
            }
            let left = WAIT_LIMIT.saturating_sub(start.elapsed());
            if left.is_zero() {
                h.alone_waiting -= usize::from(alone);
                RELEASED.notify_all();
                return Err(Error::Lock("gave up waiting for another thread after an hour".into()));
            }
            h = RELEASED.wait_timeout(h, left).unwrap_or_else(|p| p.into_inner()).0;
        }
        h.alone_waiting -= usize::from(alone);
        h.holders.push(Holder { thread: me, count: 1, sharers });
        let lock = lock();
        if h.file.is_some() {
            return Ok(lock);
        }
        // The first holder takes the file, without holding the mutex, so other threads' waits
        // aren't blocked behind another process; those that would join wait for it (`taking`).
        h.taking = true;
        drop(h);
        let taken = take(who, sharers > 1);
        let mut h = hold();
        h.taking = false;
        RELEASED.notify_all();
        match taken {
            Ok(file) => {
                h.file = Some(file);
                Ok(lock)
            }
            Err(e) => {
                // `lock` hasn't been handed out; its drop gives the hold back.
                drop(h);
                drop(lock);
                Err(e)
            }
        }
    }
}

impl Drop for GpuLock {
    fn drop(&mut self) {
        let me = std::thread::current().id();
        let mut h = hold();
        let i = h.holders.iter().position(|x| x.thread == me).expect("this thread's hold");
        h.holders[i].count -= 1;
        if h.holders[i].count == 0 {
            h.holders.swap_remove(i);
            if h.holders.is_empty() {
                h.file = None;
            }
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

/// Takes the file's `flock`, shared or alone, waiting (up to an hour) for other processes.
fn take(who: &str, shared: bool) -> Result<File> {
    let path = lock_path();
    let mut file = open(&path)?;
    let start = Instant::now();
    let mut announced = false;
    loop {
        let tried = if shared { file.try_lock_shared() } else { file.try_lock() };
        match tried {
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
    use std::sync::mpsc;

    /// The tests here use the process's one hold, so they take turns.
    static TURNS: Mutex<()> = Mutex::new(());

    fn turn() -> MutexGuard<'static, ()> {
        TURNS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn a_thread_shares_its_hold_and_others_wait() {
        let _turn = turn();
        let a = GpuLock::alone("lock test").expect("lock");
        // A second acquisition on the same thread mustn't wait on the first.
        let b = GpuLock::acquire("lock test").expect("lock");
        let me = std::thread::current().id();
        let h = hold();
        assert_eq!(h.holders.len(), 1);
        assert_eq!((h.holders[0].thread, h.holders[0].count), (me, 2));
        drop(h);
        // Another thread waits until both are gone.
        let got = AtomicBool::new(false);
        std::thread::scope(|s| {
            let waiter = s.spawn(|| {
                let _l = GpuLock::acquire_sharing("lock test, the other thread", 4).expect("lock");
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
        assert!(h.holders.is_empty() && h.file.is_none() && !h.taking && h.alone_waiting == 0);
    }

    #[test]
    fn sharers_hold_it_together_and_a_thread_alone_waits_for_them() {
        let _turn = turn();
        let a = GpuLock::acquire_sharing("lock test", 2).expect("lock");
        let alone = AtomicBool::new(false);
        std::thread::scope(|s| {
            let (joined, release) = (mpsc::channel(), mpsc::channel::<()>());
            let sharer = s.spawn(move || {
                let _l = GpuLock::acquire_sharing("lock test, a sharer", 2).expect("lock");
                joined.0.send(()).expect("send");
                release.1.recv().expect("recv");
            });
            joined.1.recv_timeout(Duration::from_secs(10)).expect("a second sharer joins at once");
            // A third sharer would be one too many, and a thread alone waits for both.
            let third = s.spawn(|| {
                let _l = GpuLock::alone("lock test, alone").expect("lock");
                alone.store(true, Ordering::SeqCst);
            });
            std::thread::sleep(Duration::from_millis(200));
            assert!(!alone.load(Ordering::SeqCst), "took the lock alone beside two sharers");
            drop(a);
            std::thread::sleep(Duration::from_millis(100));
            assert!(!alone.load(Ordering::SeqCst), "took the lock alone beside a sharer");
            release.0.send(()).expect("send");
            sharer.join().expect("the sharer");
            third.join().expect("the thread alone");
        });
        assert!(alone.load(Ordering::SeqCst));
        let h = hold();
        assert!(h.holders.is_empty() && h.file.is_none() && !h.taking && h.alone_waiting == 0);
    }

    #[test]
    fn a_sharer_that_comes_after_a_thread_waiting_alone_waits_behind_it() {
        let _turn = turn();
        let first = GpuLock::acquire_sharing("lock test", 4).expect("lock");
        let order = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            let alone = s.spawn(|| {
                let _l = GpuLock::alone("lock test, alone").expect("lock");
                order.lock().expect("order").push("alone");
            });
            // Until the thread is waiting alone.
            while hold().alone_waiting == 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
            let late = s.spawn(|| {
                let _l = GpuLock::acquire_sharing("lock test, a late sharer", 4).expect("lock");
                order.lock().expect("order").push("sharer");
            });
            std::thread::sleep(Duration::from_millis(200));
            assert!(order.lock().expect("order").is_empty(), "a sharer joined past the queue");
            drop(first);
            alone.join().expect("the thread alone");
            late.join().expect("the late sharer");
        });
        assert_eq!(*order.lock().expect("order"), ["alone", "sharer"]);
        let h = hold();
        assert!(h.holders.is_empty() && h.file.is_none() && !h.taking && h.alone_waiting == 0);
    }

    #[test]
    fn sharing_is_off_unless_the_variable_asks_for_two_or_more() {
        assert_eq!(sharers_from(None), 1);
        assert_eq!(sharers_from(Some("4")), 4);
        assert_eq!(sharers_from(Some(" 3 ")), 3);
        assert_eq!(sharers_from(Some("0")), 1);
        assert_eq!(sharers_from(Some("many")), 1);
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
