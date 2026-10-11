//! Whether wrela text is lexed outside the builds the gate's memory of slow tests records
//! (tools/memo.py; compiler/tests' `recorded_build`). The memory keys such a test on the files its
//! builds made rather than on the compiler's sources, which holds only if the compiler did all its
//! work for the test in those builds. So with `WRELA_BUILD_RECORD` set, lexing anything outside
//! them adds a line saying so to that file, and the memory keys the test on the compiler's
//! sources as well.

use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

thread_local! {
    static INSIDE: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` as a recorded build: what it lexes on this thread is the build's, and so is what
/// it lexes on a thread it starts with [`inside`]'s answer passed to [`enter`].
pub fn build<T>(f: impl FnOnce() -> T) -> T {
    let was = INSIDE.replace(true);
    let out = f();
    INSIDE.set(was);
    out
}

/// Whether this thread is in a recorded build.
pub fn inside() -> bool {
    INSIDE.get()
}

/// Makes this thread part of the recorded build its starter is in, if `inside`.
pub fn enter(inside: bool) {
    INSIDE.set(inside);
}

/// Called by each lexing: notes it in the record if it's outside a recorded build.
pub(crate) fn lexing() {
    static RECORD: OnceLock<Option<std::ffi::OsString>> = OnceLock::new();
    static SAID: AtomicBool = AtomicBool::new(false);
    let Some(path) = RECORD.get_or_init(|| std::env::var_os("WRELA_BUILD_RECORD")) else {
        return;
    };
    if INSIDE.get() || SAID.swap(true, Ordering::Relaxed) {
        return;
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path);
    if let Ok(mut f) = file {
        let _ = std::io::Write::write_all(&mut f, b"{\"outside\": true}\n");
    }
}
