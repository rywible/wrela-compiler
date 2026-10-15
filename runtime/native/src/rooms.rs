//! A room kept durably (language.md §12): a [`Room`] of a server build, its journal on disk, and
//! the rules that make it survive a crash. The journal holds the records its program gave the
//! host (`platform::net`'s `keep` and `compact`); the host keeps a call's records, flushed to
//! disk, before it releases anything the call sent or closed, so what any peer saw committed is
//! in the journal. A room that starts reads its journal back first, as messages from connection
//! `JOURNAL` (0), before any live event.
//!
//! A call that fails says whose the failure is ([`Failure`]):
//! - the program's own trap: the room starts again from its journal, then takes the call's
//!   events again one at a time; the one that traps alone is given back with `POISONED` set on
//!   its connection, so the program refuses it durably without running it. A trap while reading
//!   the journal back quarantines the room: it's never skipped past.
//! - the host's limits (the deadline, the memory cap, the stack): nothing is kept or sent; the
//!   room starts again and the events wait, and after [`RETRIES`] failures in a row its
//!   connections are closed, so its peers come back later.
//!
//! The journal: a header (`WRJ1`, the format's version, the server build's WASM hash, the room's
//! name), then records, each its length, its CRC-32C and its bytes. A journal of another build,
//! format or room isn't read: the room doesn't open. A record cut short or whose CRC is wrong
//! ends the journal there (a crash in the middle of a write), and it's truncated to the records
//! before it.

use crate::server::{Failure, Limits, Net, Room, ServerBuild};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The connection a room's journal comes from (`platform::net::JOURNAL`).
pub const JOURNAL: u32 = 0;
/// The bit set on a connection's number for a message that trapped the room alone
/// (`platform::net::POISONED`).
pub const POISONED: u32 = 1 << 31;
/// How many times in a row a room may fail at the host's limits before its connections close.
pub const RETRIES: u32 = 3;
/// The journal format's version.
const VERSION: u32 = 1;
const MAGIC: &[u8; 4] = b"WRJ1";
/// The largest record a journal holds: a snapshot of a room's whole state at most.
const MAX_RECORD: u32 = 64 << 20;

/// CRC-32C (Castagnoli), as storage formats use it.
pub fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x82F6_3B78 } else { crc >> 1 };
        }
    }
    !crc
}

/// A fault a test injects into the host's storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The next append fails before anything is flushed.
    Append,
    /// The next compaction fails after writing its new journal, before renaming it into place.
    Compact,
}

/// A room's journal on disk.
struct Journal {
    path: PathBuf,
    header: Vec<u8>,
    file: std::fs::File,
    fault: Option<Fault>,
}

fn host(why: impl std::fmt::Display) -> Failure {
    Failure::Host(why.to_string())
}

impl Journal {
    fn header(build: u64, room: &str) -> Vec<u8> {
        let mut h = MAGIC.to_vec();
        h.extend_from_slice(&VERSION.to_le_bytes());
        h.extend_from_slice(&build.to_le_bytes());
        h.extend_from_slice(&(room.len() as u32).to_le_bytes());
        h.extend_from_slice(room.as_bytes());
        h
    }

    /// The journal at `path` for the room `room` of the build whose hash is `build`, and its
    /// records: made if it isn't there, truncated after its last whole record.
    fn open(path: &Path, build: u64, room: &str) -> Result<(Journal, Vec<Vec<u8>>), Failure> {
        let header = Journal::header(build, room);
        if !path.exists() {
            let tmp = path.with_extension("new");
            write_synced(&tmp, &header).map_err(host)?;
            std::fs::rename(&tmp, path).map_err(host)?;
            sync_dir(path).map_err(host)?;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path).and_then(|mut f| f.read_to_end(&mut bytes)).map_err(host)?;
        if bytes.len() < header.len() || bytes[..header.len()] != header[..] {
            return Err(host(format!(
                "{} isn't this room's journal for this build (its header differs): it isn't read",
                path.display()
            )));
        }
        let mut records = Vec::new();
        let mut at = header.len();
        while let Some(rest) = bytes.get(at..at + 8) {
            let len = u32::from_le_bytes(rest[..4].try_into().expect("four bytes"));
            let crc = u32::from_le_bytes(rest[4..].try_into().expect("four bytes"));
            let Some(record) = bytes.get(at + 8..at + 8 + len as usize) else { break };
            if len > MAX_RECORD || crc32c(record) != crc {
                break;
            }
            records.push(record.to_vec());
            at += 8 + len as usize;
        }
        let file = std::fs::OpenOptions::new().append(true).open(path).map_err(host)?;
        if at < bytes.len() {
            // A write cut short: what follows the last whole record goes.
            file.set_len(at as u64).map_err(host)?;
            file.sync_all().map_err(host)?;
        }
        Ok((Journal { path: path.to_path_buf(), header, file, fault: None }, records))
    }

    fn framed(records: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for r in records {
            out.extend_from_slice(&(r.len() as u32).to_le_bytes());
            out.extend_from_slice(&crc32c(r).to_le_bytes());
            out.extend_from_slice(r);
        }
        out
    }

    /// Appends `records` and flushes them to disk.
    fn append(&mut self, records: &[Vec<u8>]) -> std::io::Result<()> {
        if self.fault == Some(Fault::Append) {
            self.fault = None;
            return Err(std::io::Error::other("an injected fault"));
        }
        self.file.write_all(&Journal::framed(records))?;
        self.file.sync_data()
    }

    /// Replaces the journal with `records` (a snapshot first): written whole beside it, flushed,
    /// then renamed into place, so a crash leaves one journal or the other.
    fn rewrite(&mut self, records: &[Vec<u8>]) -> std::io::Result<()> {
        let tmp = self.path.with_extension("new");
        let mut bytes = self.header.clone();
        bytes.extend_from_slice(&Journal::framed(records));
        write_synced(&tmp, &bytes)?;
        if self.fault == Some(Fault::Compact) {
            self.fault = None;
            return Err(std::io::Error::other("an injected fault"));
        }
        std::fs::rename(&tmp, &self.path)?;
        sync_dir(&self.path)?;
        self.file = std::fs::OpenOptions::new().append(true).open(&self.path)?;
        Ok(())
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Flushes the directory that holds `path`, so a file made or renamed there stays.
fn sync_dir(path: &Path) -> std::io::Result<()> {
    match path.parent() {
        Some(dir) => std::fs::File::open(dir)?.sync_all(),
        None => Ok(()),
    }
}

/// What a room's host sends its connections, in order: released only once the records before
/// it are kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    Send(u32, Vec<u8>),
    Close(u32),
}

/// An event waiting for the room.
#[derive(Clone, Debug)]
enum Incoming {
    Opened(u32),
    Closed(u32),
    Message(u32, Vec<u8>),
}

/// Whether a room is running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Live,
    /// It failed at the host's limits this many times in a row, and starts again at its next
    /// wake.
    Unavailable(u32),
    /// Its journal traps it: it stays closed.
    Quarantined(String),
}

/// How a call failed: the room (its program, or the host's limits on it), or the journal.
enum Fail {
    Room(Failure),
    Journal(Failure),
}

/// A room of a server build, kept durably in a directory of its own.
pub struct RoomHost<'b> {
    build: &'b ServerBuild,
    build_hash: u64,
    name: String,
    limits: Limits,
    journal: Journal,
    room: Option<Room>,
    status: Status,
    /// Its open connections, which a new instance is told of again.
    conns: Vec<u32>,
    incoming: VecDeque<Incoming>,
    outbox: Vec<Out>,
    /// How many calls in a row failed at the host's limits.
    failures: u32,
}

impl<'b> RoomHost<'b> {
    /// Opens room `name` of `build` (whose WASM hash is `build_hash`) in `dir`: its journal is
    /// read back into a new instance, within `limits`. A journal of another build or room isn't
    /// read, and a room its own journal traps is quarantined.
    pub fn open(
        build: &'b ServerBuild,
        build_hash: u64,
        dir: &Path,
        name: &str,
        limits: Limits,
    ) -> Result<RoomHost<'b>, Failure> {
        std::fs::create_dir_all(dir).map_err(host)?;
        let (journal, _) = Journal::open(&dir.join(format!("{name}.journal")), build_hash, name)?;
        let mut room = RoomHost {
            build,
            build_hash,
            name: name.to_string(),
            limits,
            journal,
            room: None,
            status: Status::Unavailable(0),
            conns: Vec::new(),
            incoming: VecDeque::new(),
            outbox: Vec::new(),
            failures: 0,
        };
        room.start()?;
        Ok(room)
    }

    /// Injects a fault into its storage, for a test.
    pub fn inject(&mut self, fault: Fault) {
        self.journal.fault = Some(fault);
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// The journal's records now (each as the program packed it), read back from the disk.
    pub fn journal(&self) -> Result<Vec<Vec<u8>>, Failure> {
        Journal::open(&self.journal.path, self.build_hash, &self.name).map(|(_, r)| r)
    }

    /// Starts a new instance: `serve_init`, then the journal read back, then the open
    /// connections told of again. A trap while reading the journal quarantines the room.
    fn start(&mut self) -> Result<(), Failure> {
        self.room = None;
        let (_, records) = Journal::open(&self.journal.path, self.build_hash, &self.name)?;
        let mut room = Room::start(self.build, self.limits, None)?;
        for r in records {
            room.push_message(JOURNAL, r);
        }
        // `serve` reads it back, a call's worth of bytes at a time, before any live event.
        let mut calls = 0;
        while room.queued() > 0 {
            calls += 1;
            match room.serve(0.0) {
                Ok(()) => {}
                Err(f) if f.is_program() => {
                    self.status = Status::Quarantined(format!("its journal traps it: {f}"));
                    return Err(f);
                }
                Err(f) => {
                    self.fail_at_limits();
                    return Err(f);
                }
            }
            // Reading back keeps nothing new but a compaction, which it carries out.
            for c in room.take_commands() {
                if let Net::Compact(snapshot) = c {
                    self.journal.rewrite(&[snapshot]).map_err(host)?;
                }
            }
            assert!(calls < 1 << 20, "a journal that never ends");
        }
        for &c in &self.conns {
            room.push_connection(c, true);
        }
        self.room = Some(room);
        self.status = Status::Live;
        Ok(())
    }

    fn fail_at_limits(&mut self) {
        self.room = None;
        self.failures += 1;
        if self.failures >= RETRIES {
            // It can't get through what's waiting: its peers go, and come back later.
            self.failures = 0;
            self.incoming.clear();
            for c in self.conns.drain(..) {
                self.outbox.push(Out::Close(c));
            }
            self.status = Status::Unavailable(0);
        } else {
            self.status = Status::Unavailable(self.failures);
        }
    }

    /// Connection `conn` opened.
    pub fn connect(&mut self, conn: u32) {
        assert!(conn != JOURNAL && conn < POISONED, "connection {conn} is reserved");
        self.conns.push(conn);
        self.incoming.push_back(Incoming::Opened(conn));
    }

    /// Connection `conn` closed.
    pub fn disconnect(&mut self, conn: u32) {
        self.conns.retain(|&c| c != conn);
        self.incoming.push_back(Incoming::Closed(conn));
    }

    /// A message from connection `conn`.
    pub fn receive(&mut self, conn: u32, bytes: Vec<u8>) {
        self.incoming.push_back(Incoming::Message(conn, bytes));
    }

    /// What it has released to send or close, in order.
    pub fn take_outbox(&mut self) -> Vec<Out> {
        std::mem::take(&mut self.outbox)
    }

    /// Gives the room what's waiting, at time `time` (seconds), and carries out what it does:
    /// its records kept, then its sends and closes released.
    pub fn wake(&mut self, time: f32) -> Result<(), Failure> {
        if let Status::Quarantined(why) = &self.status {
            return Err(Failure::Host(format!("the room is quarantined: {why}")));
        }
        if self.room.is_none() {
            self.start()?;
        }
        let batch: Vec<Incoming> = self.incoming.drain(..).collect();
        match self.call(&batch, time) {
            Ok(()) => Ok(()),
            // The journal failed: its peers were let go, and send again what wasn't settled.
            Err(Fail::Journal(f)) => Err(f),
            Err(Fail::Room(f)) if f.is_program() => self.isolate(batch, time),
            Err(Fail::Room(f)) => {
                // The host's limits: nothing was kept or sent, and the events wait.
                for e in batch.into_iter().rev() {
                    self.incoming.push_front(e);
                }
                self.fail_at_limits();
                Err(f)
            }
        }
    }

    /// One call with `batch`, its commands carried out if it returns.
    fn call(&mut self, batch: &[Incoming], time: f32) -> Result<(), Fail> {
        let room = self.room.as_mut().expect("a live room");
        for e in batch {
            match e {
                Incoming::Opened(c) => room.push_connection(*c, true),
                Incoming::Closed(c) => room.push_connection(*c, false),
                Incoming::Message(c, b) => room.push_message(*c, b.clone()),
            }
        }
        if let Err(f) = room.serve(time) {
            // What the failed call asked for is dropped with its instance.
            self.room = None;
            return Err(Fail::Room(f));
        }
        let commands = room.take_commands();
        self.carry_out(commands).map_err(Fail::Journal)?;
        self.failures = 0;
        Ok(())
    }

    /// Keeps a call's records (and a compaction), then releases its sends and closes. If the
    /// records can't be kept, nothing is released and the room starts again from its journal.
    fn carry_out(&mut self, commands: Vec<Net>) -> Result<(), Failure> {
        let mut records = Vec::new();
        let mut held = Vec::new();
        let mut kept = Ok(());
        for c in commands {
            match c {
                Net::Keep(r) => records.push(r),
                Net::Compact(snapshot) => {
                    // The records before it go into the journal first, then the snapshot
                    // replaces them all.
                    kept = kept.and_then(|()| self.journal.append(&std::mem::take(&mut records)));
                    kept = kept.and_then(|()| self.journal.rewrite(&[snapshot]));
                }
                Net::Send(conn, bytes) => held.push(Out::Send(conn, bytes)),
                Net::Close(conn) => held.push(Out::Close(conn)),
            }
        }
        if !records.is_empty() {
            kept = kept.and_then(|()| self.journal.append(&records));
        }
        if let Err(e) = kept {
            // Nothing the call sent goes, and its peers come back to a room started again
            // from what was kept: they send what wasn't settled again.
            self.room = None;
            self.status = Status::Unavailable(0);
            self.incoming.clear();
            for c in self.conns.drain(..) {
                self.outbox.push(Out::Close(c));
            }
            return Err(host(format!("the journal: {e}")));
        }
        for o in &held {
            if let Out::Close(c) = o {
                self.conns.retain(|x| x != c);
            }
        }
        self.outbox.extend(held);
        Ok(())
    }

    /// The program trapped on `batch`: a new instance takes its events one at a time, and the
    /// message that traps alone is given back poisoned, for the program to refuse. A new
    /// instance follows each trap; the rest go on in the same one.
    fn isolate(&mut self, batch: Vec<Incoming>, time: f32) -> Result<(), Failure> {
        for e in batch {
            if self.room.is_none() {
                self.start()?;
            }
            match self.call(std::slice::from_ref(&e), time) {
                Ok(()) => {}
                Err(Fail::Journal(f)) => return Err(f),
                Err(Fail::Room(f)) if f.is_program() => {
                    self.start()?;
                    if let Incoming::Message(c, bytes) = e {
                        // Refused without running it; if even that traps, the room can't go on.
                        match self.call(&[Incoming::Message(c | POISONED, bytes)], time) {
                            Ok(()) => {}
                            Err(Fail::Journal(f)) => return Err(f),
                            Err(Fail::Room(f)) => {
                                self.status = Status::Quarantined(format!(
                                    "a poisoned message traps it: {f}"
                                ));
                                return Err(f);
                            }
                        }
                    }
                }
                Err(Fail::Room(f)) => {
                    self.fail_at_limits();
                    return Err(f);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::crc32c;

    /// The check value every CRC-32C implementation gives.
    #[test]
    fn crc32c_is_castagnolis() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b""), 0);
    }
}
