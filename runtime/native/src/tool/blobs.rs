//! A tool's blobs (`std::tool::blob`, language.md §15.5): bytes the host holds for the tool,
//! named by handles, within a budget of bytes and handles checked before anything is allocated.
//!
//! Each blob is registered under the request that made it as it's made, before the answer that
//! names it reaches the tool: until the tool reads that answer, the blob is the request's, and a
//! request that ends unread (the tool ended first) has its blobs released with the rest.
//!
//! The table's lock is held for memory work alone, never for a file's: a service that writes a
//! blob to a file takes a handle to its bytes ([`Blobs::get`]) and writes after letting go.
//! While it writes, the blob can't be appended to or released, so its bytes stay counted.

use std::collections::HashMap;
use std::sync::Arc;

/// The most a tool's blobs may hold at once: bytes, and live handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlobBudget {
    pub bytes: u64,
    pub handles: u32,
}

impl Default for BlobBudget {
    /// 2 GiB in at most 4096 handles.
    fn default() -> BlobBudget {
        BlobBudget { bytes: 2 << 30, handles: 4096 }
    }
}

impl BlobBudget {
    /// A budget as `--blob-budget` gives it: bytes, with `K`, `M` or `G` for KiB, MiB or GiB,
    /// then optionally `,` and the handles: `64M`, `2G,16`.
    pub fn parse(text: &str) -> Result<BlobBudget, String> {
        let bad = || format!("`{text}` isn't a blob budget: bytes (`64M`), then `,` and handles");
        let (bytes, handles) = match text.split_once(',') {
            Some((b, h)) => (b, Some(h)),
            None => (text, None),
        };
        let (digits, scale) = match bytes.as_bytes().last() {
            Some(b'K' | b'k') => (&bytes[..bytes.len() - 1], 1u64 << 10),
            Some(b'M' | b'm') => (&bytes[..bytes.len() - 1], 1 << 20),
            Some(b'G' | b'g') => (&bytes[..bytes.len() - 1], 1 << 30),
            _ => (bytes, 1),
        };
        let n: u64 = digits.parse().map_err(|_| bad())?;
        let bytes = n.checked_mul(scale).ok_or_else(bad)?;
        let handles = match handles {
            Some(h) => h.parse().map_err(|_| bad())?,
            None => BlobBudget::default().handles,
        };
        Ok(BlobBudget { bytes, handles })
    }
}

/// The blobs of one run of a tool.
#[derive(Debug, Default)]
pub(crate) struct Blobs {
    held: HashMap<u32, Arc<Vec<u8>>>,
    /// The bytes all blobs hold.
    bytes: u64,
    next: u32,
    budget: BlobBudget,
    /// Each request's blobs that the tool hasn't been told of yet.
    unpublished: HashMap<u32, Vec<u32>>,
}

impl Blobs {
    pub(crate) fn new(budget: BlobBudget) -> Blobs {
        Blobs { budget, next: 1, ..Blobs::default() }
    }

    /// A new, empty blob, made for `request`.
    pub(crate) fn create(&mut self, request: u32) -> Result<u32, String> {
        if self.held.len() as u64 >= u64::from(self.budget.handles) {
            return Err(format!(
                "the tool holds {} blobs, as many as its budget allows: release one",
                self.held.len()
            ));
        }
        let id = self.next;
        self.next += 1;
        self.held.insert(id, Arc::new(Vec::new()));
        self.unpublished.entry(request).or_default().push(id);
        Ok(id)
    }

    /// Whether `more` bytes fit the budget beside what the blobs hold.
    fn reserve(&self, more: u64) -> Result<(), String> {
        if self.bytes.saturating_add(more) > self.budget.bytes {
            return Err(format!(
                "{more} more bytes would take the tool's blobs past their budget of {} bytes",
                self.budget.bytes
            ));
        }
        Ok(())
    }

    /// Adds `bytes` to the end of blob `id`, within the budget.
    pub(crate) fn append(&mut self, id: u32, bytes: &[u8]) -> Result<(), String> {
        self.reserve(bytes.len() as u64)?;
        let blob = self.held.get_mut(&id).ok_or_else(|| no_blob(id))?;
        // A blob being written to a file is shared with the writer: it isn't copied to grow.
        let Some(blob) = Arc::get_mut(blob) else {
            return Err(format!("blob {id} is being written to a file: append once that's done"));
        };
        blob.extend_from_slice(bytes);
        self.bytes += bytes.len() as u64;
        Ok(())
    }

    /// Checks that `more` bytes fit the budget now, before they're read.
    pub(crate) fn fits(&self, more: u64) -> Result<(), String> {
        self.reserve(more)
    }

    /// Up to `len` bytes of blob `id` from `offset`.
    pub(crate) fn read(&self, id: u32, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        let blob = self.held.get(&id).ok_or_else(|| no_blob(id))?;
        if offset > blob.len() as u64 {
            return Err(format!(
                "the offset {offset} is past the end of blob {id}, which holds {} bytes",
                blob.len()
            ));
        }
        let start = offset as usize;
        let end = start.saturating_add(len).min(blob.len());
        Ok(blob[start..end].to_vec())
    }

    /// Blob `id`'s bytes, shared: for work done after the table's lock is let go.
    pub(crate) fn get(&self, id: u32) -> Result<Arc<Vec<u8>>, String> {
        self.held.get(&id).cloned().ok_or_else(|| no_blob(id))
    }

    /// Frees blob `id`. A blob being written to a file is refused, as an append to it is: its
    /// bytes stay charged to the budget until the write is done with them.
    pub(crate) fn release(&mut self, id: u32) -> Result<(), String> {
        let blob = self.held.get(&id).ok_or_else(|| no_blob(id))?;
        if Arc::strong_count(blob) > 1 {
            return Err(format!(
                "blob {id} is being written to a file: release it once that's done"
            ));
        }
        let blob = self.held.remove(&id).ok_or_else(|| no_blob(id))?;
        self.bytes -= blob.len() as u64;
        for ids in self.unpublished.values_mut() {
            ids.retain(|&b| b != id);
        }
        Ok(())
    }

    /// Request `request` failed: the blobs made for it, which no answer names, are released,
    /// and their bytes freed. How many there were.
    pub(crate) fn rollback(&mut self, request: u32) -> usize {
        let ids = self.unpublished.remove(&request).unwrap_or_default();
        for &id in &ids {
            if let Some(blob) = self.held.remove(&id) {
                self.bytes -= blob.len() as u64;
            }
        }
        ids.len()
    }

    /// The tool has read request `request`'s answer: its blobs are the tool's now.
    pub(crate) fn publish(&mut self, request: u32) {
        self.unpublished.remove(&request);
    }

    /// Releases every blob, unread requests' first: how many there were, and their bytes.
    pub(crate) fn release_all(&mut self) -> (usize, u64) {
        let ended = (self.held.len(), self.bytes);
        for (_, ids) in std::mem::take(&mut self.unpublished) {
            for id in ids {
                self.held.remove(&id);
            }
        }
        self.held.clear();
        self.bytes = 0;
        ended
    }
}

fn no_blob(id: u32) -> String {
    format!("the tool holds no blob {id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_is_checked_before_bytes_or_handles_are_allocated() {
        let mut b = Blobs::new(BlobBudget { bytes: 10, handles: 2 });
        let one = b.create(1).expect("a blob");
        let _two = b.create(2).expect("another");
        assert!(b.create(3).unwrap_err().contains("budget"));
        b.append(one, &[1; 8]).expect("8 bytes fit");
        assert!(b.append(one, &[1; 3]).unwrap_err().contains("budget"));
        assert_eq!(b.get(one).expect("held").len(), 8, "a refused append adds nothing");
        b.release(one).expect("released");
        b.create(4).expect("a handle is free again");
        assert_eq!(b.release_all(), (2, 0));
    }

    #[test]
    fn a_blob_being_written_stays_charged_until_the_write_lets_go() {
        let mut b = Blobs::new(BlobBudget { bytes: 8, handles: 4 });
        let one = b.create(1).expect("a blob");
        b.append(one, &[1; 8]).expect("8 bytes");
        // A write to a file holds the bytes.
        let writing = b.get(one).expect("held");
        assert!(b.release(one).unwrap_err().contains("being written"));
        let two = b.create(2).expect("a handle");
        assert!(b.append(two, &[2; 8]).unwrap_err().contains("budget"), "the 8 bytes are held");
        drop(writing);
        b.release(one).expect("released once the write is done");
        b.append(two, &[2; 8]).expect("the budget is free again");
    }

    #[test]
    fn a_failed_requests_blobs_are_released_with_their_bytes() {
        let mut b = Blobs::new(BlobBudget { bytes: 10, handles: 4 });
        let one = b.create(1).expect("a blob");
        b.append(one, &[1; 6]).expect("6 bytes");
        let kept = b.create(2).expect("another request's");
        assert_eq!(b.rollback(1), 1);
        assert!(b.get(one).is_err(), "released");
        b.append(kept, &[2; 10]).expect("its bytes are free again");
        assert_eq!(b.rollback(1), 0, "once");
    }

    #[test]
    fn a_budget_reads_as_the_command_line_gives_it() {
        assert_eq!(BlobBudget::parse("64M"), Ok(BlobBudget { bytes: 64 << 20, handles: 4096 }));
        assert_eq!(BlobBudget::parse("2G,16"), Ok(BlobBudget { bytes: 2 << 30, handles: 16 }));
        assert_eq!(BlobBudget::parse("100"), Ok(BlobBudget { bytes: 100, handles: 4096 }));
        assert!(BlobBudget::parse("lots").is_err());
    }
}
