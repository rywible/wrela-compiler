//! A tool's console (`std::io::print`, language.md §15.5): a call into the tool never waits on
//! it. The print host call puts the line on a bounded queue and returns; a thread of the
//! host's own hands each line to the printer. A line that finds the queue full (at most
//! [`LINES`] lines and [`BYTES`] bytes) is dropped and counted, and the writer says how many it
//! dropped once the printer takes a line again. A printer that never returns (standard output
//! piped to a reader that doesn't read) holds up the writer alone: when the tool ends, the host
//! gives the writer a grace to drain the queue, then leaves it, and the lines it didn't print
//! count as dropped.

use super::Printer;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// The most lines the queue holds.
pub const LINES: usize = 1024;

/// The most bytes of lines the queue holds.
pub const BYTES: usize = 4 << 20;

/// What the tool's store holds, and each of its helpers: the queue's sending end.
#[derive(Clone)]
pub(crate) struct Console {
    lines: mpsc::SyncSender<String>,
    counts: Arc<Counts>,
}

#[derive(Default)]
struct Counts {
    /// The bytes of the lines queued.
    queued_bytes: AtomicUsize,
    /// The lines queued.
    queued: AtomicUsize,
    /// The lines dropped and not yet reported.
    dropped: AtomicU64,
    /// Every line dropped.
    dropped_ever: AtomicU64,
}

impl Counts {
    fn drop_lines(&self, n: u64) {
        self.dropped.fetch_add(n, Ordering::AcqRel);
        self.dropped_ever.fetch_add(n, Ordering::AcqRel);
    }
}

/// The thread that writes the lines out.
pub(crate) struct Writer {
    finished: mpsc::Receiver<()>,
    handle: Option<std::thread::JoinHandle<()>>,
    counts: Arc<Counts>,
}

impl Console {
    /// The queue, and its writer handing lines to `print`, or why the writer can't start.
    pub(crate) fn start(print: Printer) -> Result<(Console, Writer), String> {
        let (lines, queue) = mpsc::sync_channel::<String>(LINES);
        let counts = Arc::new(Counts::default());
        let (finish, finished) = mpsc::channel();
        let c = counts.clone();
        let handle = std::thread::Builder::new()
            .name("wrela-tool-console".into())
            .spawn(move || {
                for line in queue {
                    c.queued.fetch_sub(1, Ordering::AcqRel);
                    c.queued_bytes.fetch_sub(line.len(), Ordering::AcqRel);
                    print(&line);
                    let n = c.dropped.swap(0, Ordering::AcqRel);
                    if n > 0 {
                        print(&format!(
                            "[the tool host dropped {n} lines the console didn't take in time]"
                        ));
                    }
                }
                let _ = finish.send(());
            })
            .map_err(|e| format!("can't start the tool's console: {e}"))?;
        Ok((
            Console { lines, counts: counts.clone() },
            Writer { finished, handle: Some(handle), counts },
        ))
    }

    /// Queues `line` without waiting: a full queue drops it. The tool's thread and its helpers
    /// print at once, so a line's bytes are reserved in one step, below the cap, before it's
    /// queued.
    pub(crate) fn print(&self, line: String) {
        let c = &self.counts;
        let len = line.len();
        let reserved = c.queued_bytes.fetch_update(Ordering::AcqRel, Ordering::Acquire, |q| {
            q.checked_add(len).filter(|&n| n <= BYTES)
        });
        if reserved.is_err() {
            c.drop_lines(1);
            return;
        }
        c.queued.fetch_add(1, Ordering::AcqRel);
        if self.lines.try_send(line).is_err() {
            c.queued.fetch_sub(1, Ordering::AcqRel);
            c.queued_bytes.fetch_sub(len, Ordering::AcqRel);
            c.drop_lines(1);
        }
    }
}

impl Writer {
    /// Waits up to `grace` for the writer to print what's queued (the queue's sending end must
    /// be gone), then leaves it: every line dropped, those still queued among them.
    pub(crate) fn stop(mut self, grace: Duration) -> u64 {
        if self.finished.recv_timeout(grace).is_ok() {
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        } else {
            self.counts.drop_lines(self.counts.queued.load(Ordering::Acquire) as u64);
        }
        self.counts.dropped_ever.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Instant;

    #[test]
    fn a_printer_that_never_returns_holds_up_no_print_and_no_end() {
        let (release, held) = mpsc::channel::<()>();
        let held = Mutex::new(held);
        let printed = Arc::new(AtomicUsize::new(0));
        let p = printed.clone();
        // The first line it's given, it never returns from (until the test ends).
        let (console, writer) = Console::start(Arc::new(move |_| {
            p.fetch_add(1, Ordering::AcqRel);
            let _ = held.lock().map(|h| h.recv());
        }))
        .expect("a console");
        let started = Instant::now();
        for i in 0..LINES * 3 {
            console.print(format!("line {i}"));
        }
        assert!(started.elapsed() < Duration::from_secs(1), "printing never waits");
        drop(console);
        let dropped = writer.stop(Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(2), "the end is bounded");
        assert_eq!(printed.load(Ordering::Acquire), 1);
        // One line printed, the rest dropped: those that found the queue full, and the queued.
        assert_eq!(dropped, (LINES * 3 - 1) as u64);
        drop(release);
    }

    #[test]
    fn producers_at_once_never_queue_more_than_the_byte_cap() {
        // A helper prints beside the tool: lines just over half the cap, from 8 threads at once,
        // with the printer held on its first line. One fits; every other is dropped and counted.
        for _ in 0..1000 {
            let (release, held) = mpsc::channel::<()>();
            let held = Mutex::new(held);
            let taken = Arc::new(AtomicUsize::new(0));
            let t = taken.clone();
            let (console, writer) = Console::start(Arc::new(move |_| {
                t.fetch_add(1, Ordering::AcqRel);
                let _ = held.lock().map(|h| h.recv());
            }))
            .expect("a console");
            console.print("first".into());
            while taken.load(Ordering::Acquire) == 0 {
                std::thread::yield_now();
            }
            let line = "x".repeat(BYTES / 2 + 1);
            // They spin until all are ready, then print together.
            let ready = Arc::new(AtomicUsize::new(0));
            let producers: Vec<_> = (0..8)
                .map(|_| {
                    let (console, line, ready) = (console.clone(), line.clone(), ready.clone());
                    std::thread::spawn(move || {
                        ready.fetch_add(1, Ordering::AcqRel);
                        while ready.load(Ordering::Acquire) < 8 {
                            std::hint::spin_loop();
                        }
                        console.print(line);
                    })
                })
                .collect();
            for p in producers {
                p.join().expect("a producer");
            }
            let c = &console.counts;
            assert!(c.queued_bytes.load(Ordering::Acquire) <= BYTES, "over the cap");
            assert_eq!(c.queued.load(Ordering::Acquire), 1, "one line fits");
            assert_eq!(c.dropped_ever.load(Ordering::Acquire), 7, "the rest are counted");
            drop(console);
            drop(release);
            writer.stop(Duration::from_secs(5));
        }
    }

    #[test]
    fn every_line_is_printed_in_order_when_the_printer_keeps_up() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let l = lines.clone();
        let (console, writer) = Console::start(Arc::new(move |line: &str| {
            l.lock().expect("lines").push(line.to_string());
        }))
        .expect("a console");
        for i in 0..10 {
            console.print(format!("{i}"));
        }
        drop(console);
        assert_eq!(writer.stop(Duration::from_secs(5)), 0);
        let want: Vec<String> = (0..10).map(|i| i.to_string()).collect();
        assert_eq!(*lines.lock().expect("lines"), want);
    }
}
