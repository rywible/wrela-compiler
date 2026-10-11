//! The program's memory, which its workers share: the one place this host reads or writes it
//! (wasmtime gives a shared memory's bytes as `UnsafeCell`s, so this is the host's only
//! `unsafe` code).
//!
//! The program's own thread reads a batch, or writes a request's answer or clears a panic
//! message, only inside a call it makes to the host, while no parallel job runs: so no worker
//! touches those bytes meanwhile. The words a host writes for its other threads (a helper's
//! trap: [`wrela_abi::memory::JOB_FAILED`], a slot's state; [`wrela_abi::memory::PAR_SHUTDOWN`],
//! the ticker's) are written atomically, as those threads read them. A tick's records are
//! written before the call that reads them, while the ticker's thread waits for it.
#![allow(unsafe_code)]

use std::sync::atomic::{AtomicU32, Ordering};
use wasmtime::SharedMemory;

/// Bytes `at..at + len`, if they're inside the memory: read in place, so only while no other
/// thread writes them (above).
pub fn slice(m: &SharedMemory, at: usize, len: usize) -> Option<&[u8]> {
    let data = m.data();
    let end = at.checked_add(len).filter(|&e| e <= data.len())?;
    let cells = &data[at..end];
    // SAFETY: `UnsafeCell<u8>` has the layout of `u8`, the memory lives as long as `m` (it never
    // moves), and no other thread writes these bytes now (above).
    Some(unsafe { std::slice::from_raw_parts(cells.as_ptr().cast::<u8>(), cells.len()) })
}

/// A copy of bytes `at..at + len`, if they're inside the memory.
pub(crate) fn read(m: &SharedMemory, at: usize, len: usize) -> Option<Vec<u8>> {
    slice(m, at, len).map(<[u8]>::to_vec)
}

/// Writes `bytes` at `at`: whether they fit.
pub fn write(m: &SharedMemory, at: usize, bytes: &[u8]) -> bool {
    let data = m.data();
    let Some(end) = at.checked_add(bytes.len()).filter(|&e| e <= data.len()) else {
        return false;
    };
    if at < end {
        // SAFETY: as for `slice`.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), data[at].get(), bytes.len()) };
    }
    true
}

/// The word at `at` (4-byte aligned, inside the memory), as an atomic.
fn word(m: &SharedMemory, at: usize) -> &AtomicU32 {
    let data = m.data();
    assert!(at.is_multiple_of(4) && at + 4 <= data.len(), "a word inside the memory");
    // SAFETY: an aligned word inside the memory, which lives as long as `m`; the program's
    // threads read and write this word with atomic instructions only.
    unsafe { &*(data[at].get() as *const AtomicU32) }
}

/// Stores `v` at `at` atomically.
pub(crate) fn store_u32(m: &SharedMemory, at: u32, v: u32) {
    word(m, at as usize).store(v, Ordering::SeqCst);
}

/// The word at `at`, read atomically.
pub(crate) fn load_u32(m: &SharedMemory, at: u32) -> u32 {
    word(m, at as usize).load(Ordering::SeqCst)
}

/// The `u64` at `at`: its two words, low first, each read atomically.
pub(crate) fn load_u64(m: &SharedMemory, at: u32) -> u64 {
    u64::from(load_u32(m, at)) | (u64::from(load_u32(m, at + 4)) << 32)
}

/// Sets `bits` in the word at `at` atomically.
pub(crate) fn or_u32(m: &SharedMemory, at: u32, bits: u32) {
    word(m, at as usize).fetch_or(bits, Ordering::SeqCst);
}

/// Adds `v` to the word at `at` atomically.
pub(crate) fn add_u32(m: &SharedMemory, at: u32, v: u32) {
    word(m, at as usize).fetch_add(v, Ordering::SeqCst);
}

/// Stores `new` at `at` atomically if the word there is `expected`: whether it did.
pub(crate) fn cas_u32(m: &SharedMemory, at: u32, expected: u32, new: u32) -> bool {
    word(m, at as usize).compare_exchange(expected, new, Ordering::SeqCst, Ordering::SeqCst).is_ok()
}
