//! A room kept durably (language.md §19): a [`Room`] of a server build, its journal on disk, and
//! the rules that make it survive a crash. The journal holds the records its program gave the
//! host (`std::net`'s `keep` and `compact`); the host keeps a call's records, flushed to
//! disk, before it releases anything the call sent or closed, so what any peer saw committed is
//! in the journal. A room that starts reads its journal back first, as messages from connection
//! `JOURNAL` (0), before any live event, and then hears of its open connections again, each with
//! the tag the program gave it (`std::net::tag`). A call is given what waits a call's worth
//! at a time ([`wrela_abi::input::within_call`]), and must read all of it.
//!
//! A call that fails keeps nothing and sends nothing, and the room starts again from its journal
//! and takes the call's events again one at a time, so one event can't fail the rest. Alone, an
//! event's failure says whose it is ([`Failure`]):
//! - the program's own trap (or a `serve` that returns with events unread): the event, if it's
//!   a message, is given back with `POISONED` set on its connection, so the program refuses it
//!   durably without running it. A trap on a poisoned message, or while reading the journal
//!   back, quarantines the room: it's never skipped past, what waits goes, and its connections
//!   close.
//! - the host's limits (the deadline, the memory cap, the stack, a call's commands): the event
//!   waits, with those after it, for the next wake, and after [`RETRIES`] failures in a row the
//!   room's connections are closed, so its peers come back later.
//!
//! One room host at a time in a process holds a room's journal; another of the same room (a
//! build swapped away and back, say) waits for it to close. The journal is made with its first
//! record.
//!
//! The journal: a header (`WRJ1`, the format's version, the server build's WASM hash, the room's
//! name), then records, each its length, its CRC-32C and its bytes. A journal of another build,
//! format or room isn't read: the room doesn't open. A record cut short or whose CRC is wrong
//! ends the journal there (a crash in the middle of a write), and it's truncated to the records
//! before it.

use crate::server::{Answers, Failure, Limits, Net, Room, ServerBuild};
use crate::services::Services;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The connection a room's journal comes from (`std::net::JOURNAL`).
pub const JOURNAL: u32 = 0;
/// The bit set on a connection's number for a message that trapped the room alone
/// (`std::net::POISONED`).
pub const POISONED: u32 = 1 << 31;
/// How many times in a row a room may fail at the host's limits before its connections close.
pub const RETRIES: u32 = 3;
/// Why a call that returned without reading all its events failed.
const UNREAD: &str = "`serve` returned without reading all its events";
/// The journal format's version.
const VERSION: u32 = 1;
const MAGIC: &[u8; 4] = b"WRJ1";
/// The largest record a journal holds: a snapshot of a room's whole state at most.
const MAX_RECORD: u32 = 64 << 20;

const CRC32C_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut byte = 0;
    while byte < table.len() {
        let mut crc = byte as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x82F6_3B78 } else { crc >> 1 };
            bit += 1;
        }
        table[byte] = crc;
        byte += 1;
    }
    table
};

/// CRC-32C (Castagnoli), as storage formats use it.
pub fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc = (crc >> 8) ^ CRC32C_TABLE[((crc ^ u32::from(b)) & 255) as usize];
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
    /// The next compaction fails after renaming its new journal into place, before opening it.
    AfterRename,
}

/// A room's journal on disk: made with its first record, so a room that keeps nothing leaves
/// nothing behind.
struct Journal {
    path: PathBuf,
    header: Vec<u8>,
    file: Option<std::fs::File>,
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
    /// records (none if it isn't there yet), truncated after its last whole record.
    fn open(path: &Path, build: u64, room: &str) -> Result<(Journal, Vec<Vec<u8>>), Failure> {
        let header = Journal::header(build, room);
        if !path.exists() {
            let journal = Journal { path: path.to_path_buf(), header, file: None, fault: None };
            return Ok((journal, Vec::new()));
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
        Ok((Journal { path: path.to_path_buf(), header, file: Some(file), fault: None }, records))
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

    /// Fails, once, if a test injected `fault`.
    fn injected(&mut self, fault: Fault) -> std::io::Result<()> {
        if self.fault == Some(fault) {
            self.fault = None;
            return Err(std::io::Error::other("an injected fault"));
        }
        Ok(())
    }

    /// Appends `records` and flushes them to disk.
    fn append(&mut self, records: &[Vec<u8>]) -> std::io::Result<()> {
        self.injected(Fault::Append)?;
        let file = match &mut self.file {
            Some(file) => file,
            None => {
                // Its header, written whole beside it and renamed into place.
                let tmp = self.path.with_extension("new");
                write_synced(&tmp, &self.header)?;
                std::fs::rename(&tmp, &self.path)?;
                sync_dir(&self.path)?;
                self.file.insert(std::fs::OpenOptions::new().append(true).open(&self.path)?)
            }
        };
        file.write_all(&Journal::framed(records))?;
        file.sync_data()
    }

    /// Replaces the journal with `records` (a snapshot first): written whole beside it, flushed,
    /// then renamed into place, so a crash leaves one journal or the other.
    fn rewrite(&mut self, records: &[Vec<u8>]) -> std::io::Result<()> {
        let tmp = self.path.with_extension("new");
        let mut bytes = self.header.clone();
        bytes.extend_from_slice(&Journal::framed(records));
        write_synced(&tmp, &bytes)?;
        self.injected(Fault::Compact)?;
        std::fs::rename(&tmp, &self.path)?;
        self.injected(Fault::AfterRename)?;
        sync_dir(&self.path)?;
        self.file = Some(std::fs::OpenOptions::new().append(true).open(&self.path)?);
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

/// The journals a room host has open in this process: one room at a time keeps each.
static OPEN: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
static CLOSED: std::sync::Condvar = std::sync::Condvar::new();
/// How long a room waits for the room host before it, of the same journal, to close.
const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// A room's hold on its journal, given up when it's dropped.
struct Held(PathBuf);

impl Held {
    /// The journal at `path`, once no other room host in this process holds it (within
    /// [`WAIT`]).
    fn take(path: &Path) -> Result<Held, Failure> {
        let open = OPEN.lock().expect("the open journals' lock");
        let (mut open, waited) = CLOSED
            .wait_timeout_while(open, WAIT, |open| open.iter().any(|p| p == path))
            .expect("the open journals' lock");
        if waited.timed_out() {
            return Err(host(format!("{} is held by another room", path.display())));
        }
        open.push(path.to_path_buf());
        Ok(Held(path.to_path_buf()))
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        OPEN.lock().expect("the open journals' lock").retain(|p| *p != self.0);
        CLOSED.notify_all();
    }
}

/// A room of a server build, kept durably in a directory of its own. One room host at a time
/// holds a journal in a process: another of the same room waits for it to close.
pub struct RoomHost {
    build: std::sync::Arc<ServerBuild>,
    build_hash: u64,
    name: String,
    limits: Limits,
    /// The server's services, which answer its requests and keep its writes; none, and each
    /// request fails and a write fails the call.
    services: Option<Arc<Services>>,
    journal: Journal,
    room: Option<Room>,
    status: Status,
    /// Its open connections and their tags, which a new instance is told of again.
    conns: Vec<(u32, u32)>,
    incoming: VecDeque<Incoming>,
    outbox: Vec<Out>,
    /// How many calls in a row failed at the host's limits.
    failures: u32,
    _held: Held,
}

impl RoomHost {
    /// Opens room `name` of `build` (whose WASM hash is `build_hash`) in `dir`: its journal is
    /// read back into a new instance, within `limits`, with the server's `services`. A journal of
    /// another build or room isn't read, and a room its own journal traps is quarantined.
    pub fn open(
        build: std::sync::Arc<ServerBuild>,
        build_hash: u64,
        dir: &Path,
        name: &str,
        limits: Limits,
        services: Option<Arc<Services>>,
    ) -> Result<RoomHost, Failure> {
        std::fs::create_dir_all(dir).map_err(host)?;
        let dir = dir.canonicalize().map_err(host)?;
        let path = dir.join(format!("{name}.journal"));
        let held = Held::take(&path)?;
        let (journal, records) = Journal::open(&path, build_hash, name)?;
        let mut room = RoomHost {
            build,
            build_hash,
            name: name.to_string(),
            limits,
            services,
            journal,
            room: None,
            status: Status::Unavailable(0),
            conns: Vec::new(),
            incoming: VecDeque::new(),
            outbox: Vec::new(),
            failures: 0,
            _held: held,
        };
        room.restore(records)?;
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
    /// connections told of again, with their tags. The journal is opened again, so a rewrite
    /// that failed past its rename leaves no stale file behind. A trap while reading the journal
    /// back quarantines the room.
    fn start(&mut self) -> Result<(), Failure> {
        self.room = None;
        let (mut journal, records) =
            Journal::open(&self.journal.path, self.build_hash, &self.name)?;
        journal.fault = self.journal.fault.take();
        self.journal = journal;
        self.restore(records)
    }

    /// Starts an instance with the records from the journal just opened.
    fn restore(&mut self, records: Vec<Vec<u8>>) -> Result<(), Failure> {
        let answers: Option<Answers> = self.services.clone().map(|s| {
            let answer: Answers =
                Arc::new(move |service: &str, body: &[u8]| s.answer(service, body));
            answer
        });
        let mut room = Room::start(&self.build, self.limits, answers)?;
        for r in &records {
            room.push_message(JOURNAL, r.clone());
        }
        // `serve` reads it back, a call's worth of bytes at a time, before any live event.
        while room.queued() > 0 {
            let before = room.queued();
            let read = room.serve(0.0).and_then(|()| {
                if room.queued() < before { Ok(()) } else { Err(Failure::Program(UNREAD.into())) }
            });
            match read {
                Ok(()) => {}
                Err(f) if f.is_program() => {
                    self.quarantine(format!("its journal traps it: {f}"));
                    return Err(f);
                }
                Err(f) => return Err(f),
            }
            // Reading back keeps nothing new but a compaction, which it carries out: the
            // snapshot, then the records not read yet, which it doesn't hold. Its writes to the
            // store were committed when the records were first kept.
            for c in room.take_commands() {
                if let Net::Compact(snapshot) = c {
                    let unread = &records[records.len() - room.queued()..];
                    let kept: Vec<Vec<u8>> =
                        std::iter::once(snapshot).chain(unread.iter().cloned()).collect();
                    self.journal.rewrite(&kept).map_err(host)?;
                }
            }
        }
        for &(c, tag) in &self.conns {
            room.push_connection(c, true, tag);
        }
        self.room = Some(room);
        self.status = Status::Live;
        Ok(())
    }

    /// The room can't go on: what waits for it goes, and its connections close.
    fn quarantine(&mut self, why: String) {
        self.room = None;
        self.status = Status::Quarantined(why);
        self.close_all();
    }

    /// What waits for it goes, and its connections close.
    fn close_all(&mut self) {
        self.incoming.clear();
        for (c, _) in self.conns.drain(..) {
            self.outbox.push(Out::Close(c));
        }
    }

    fn quarantined(&self) -> bool {
        matches!(self.status, Status::Quarantined(_))
    }

    /// How many events wait for it.
    pub fn waiting(&self) -> usize {
        self.incoming.len()
    }

    fn fail_at_limits(&mut self) {
        self.room = None;
        self.failures += 1;
        if self.failures >= RETRIES {
            // It can't get through what's waiting: its peers go, and come back later.
            self.failures = 0;
            self.close_all();
            self.status = Status::Unavailable(0);
        } else {
            self.status = Status::Unavailable(self.failures);
        }
    }

    /// Connection `conn` opened: a quarantined room closes it.
    pub fn connect(&mut self, conn: u32) {
        assert!(conn != JOURNAL && conn < POISONED, "connection {conn} is reserved");
        if self.quarantined() {
            self.outbox.push(Out::Close(conn));
            return;
        }
        self.conns.push((conn, 0));
        self.incoming.push_back(Incoming::Opened(conn));
    }

    /// Connection `conn` closed.
    pub fn disconnect(&mut self, conn: u32) {
        self.conns.retain(|&(c, _)| c != conn);
        self.incoming.push_back(Incoming::Closed(conn));
    }

    /// A message from connection `conn`: one from a connection it doesn't have (closed, or
    /// let go), or for a quarantined room, goes nowhere.
    pub fn receive(&mut self, conn: u32, bytes: Vec<u8>) {
        if self.conns.iter().any(|&(c, _)| c == conn) {
            self.incoming.push_back(Incoming::Message(conn, bytes));
        }
    }

    /// What it has released to send or close, in order.
    pub fn take_outbox(&mut self) -> Vec<Out> {
        std::mem::take(&mut self.outbox)
    }

    /// Gives the room what's waiting, at time `time` (seconds), a call's worth at a time (the
    /// payload bytes a call is given, `wrela_abi::input::within_call`), and carries out what
    /// each call does: its records kept, then its sends and closes released.
    pub fn wake(&mut self, time: f32) -> Result<(), Failure> {
        if let Status::Quarantined(why) = &self.status {
            return Err(Failure::Host(format!("the room is quarantined: {why}")));
        }
        loop {
            if self.room.is_none()
                && let Err(f) = self.start()
            {
                if !self.quarantined() {
                    self.fail_at_limits();
                }
                return Err(f);
            }
            let batch = self.next_batch();
            match self.call(&batch, time) {
                Ok(()) => {}
                // The journal failed: its peers were let go, and send again what wasn't settled.
                Err(Fail::Journal(f)) => return Err(f),
                Err(Fail::Room(f)) if batch.len() == 1 && !f.is_program() => {
                    // The host's limits, on one event: nothing was kept or sent, and it waits.
                    self.wait(batch.into_iter());
                    self.fail_at_limits();
                    return Err(f);
                }
                Err(Fail::Room(_)) => self.one_by_one(batch, time)?,
            }
            if self.incoming.is_empty() {
                return Ok(());
            }
        }
    }

    /// The events one call is given: messages up to its payload bytes, and what's between them.
    fn next_batch(&mut self) -> Vec<Incoming> {
        let mut given = 0;
        let mut batch = Vec::new();
        while let Some(e) = self.incoming.front() {
            if let Incoming::Message(_, bytes) = e {
                if !wrela_abi::input::within_call(given, bytes.len()) {
                    break;
                }
                given += bytes.len();
            }
            batch.extend(self.incoming.pop_front());
        }
        batch
    }

    /// One call with `batch`, its commands carried out if it returns.
    fn call(&mut self, batch: &[Incoming], time: f32) -> Result<(), Fail> {
        let room = self.room.as_mut().expect("a live room");
        for e in batch {
            match e {
                Incoming::Opened(c) => room.push_connection(*c, true, 0),
                Incoming::Closed(c) => room.push_connection(*c, false, 0),
                Incoming::Message(c, b) => room.push_message(*c, b.clone()),
            }
        }
        // A call is given the whole batch, so one that returns without reading it all is the
        // program's failure: what it left would wait for nothing.
        let served = room.serve(time).and_then(|()| {
            if room.queued() == 0 { Ok(()) } else { Err(Failure::Program(UNREAD.into())) }
        });
        if let Err(f) = served {
            // What the failed call asked for is dropped with its instance.
            self.room = None;
            return Err(Fail::Room(f));
        }
        let commands = room.take_commands();
        self.carry_out(commands).map_err(Fail::Journal)?;
        self.failures = 0;
        Ok(())
    }

    /// Commits a call's writes to the store, keeps its records (and a compaction), then releases
    /// its sends and closes. If the writes or the records can't be kept, nothing is released and
    /// the room starts again from its journal. Writes are committed first: a crash between them
    /// and the records leaves the writes kept and the call's event not settled, so its peer sends
    /// it again, and a program's writes should be the same when they are made again.
    fn carry_out(&mut self, commands: Vec<Net>) -> Result<(), Failure> {
        let mut records = Vec::new();
        let mut held = Vec::new();
        let mut tags = Vec::new();
        let writes: Vec<&[u8]> = commands
            .iter()
            .filter_map(|c| if let Net::Write(w) = c { Some(w.as_slice()) } else { None })
            .collect();
        let mut kept = if writes.is_empty() {
            Ok(())
        } else {
            match &self.services {
                Some(s) => s.write(&writes).map_err(std::io::Error::other),
                None => Err(std::io::Error::other("this server has no store")),
            }
        };
        for c in commands {
            match c {
                Net::Write(_) => {}
                Net::Keep(r) => records.push(r),
                Net::Compact(snapshot) => {
                    // The records before it go into the journal first, then the snapshot
                    // replaces them all.
                    kept = kept.and_then(|()| self.journal.append(&std::mem::take(&mut records)));
                    kept = kept.and_then(|()| self.journal.rewrite(&[snapshot]));
                }
                Net::Send(conn, bytes) => held.push(Out::Send(conn, bytes)),
                Net::Close(conn) => held.push(Out::Close(conn)),
                Net::Tag(conn, tag) => tags.push((conn, tag)),
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
            self.close_all();
            return Err(host(format!("the journal: {e}")));
        }
        for (conn, tag) in tags {
            for c in &mut self.conns {
                if c.0 == conn {
                    c.1 = tag;
                }
            }
        }
        for o in &held {
            if let Out::Close(c) = o {
                self.conns.retain(|x| x.0 != *c);
            }
        }
        self.outbox.extend(held);
        Ok(())
    }

    /// The call on `batch` failed: a fresh instance takes its events one at a time, so one
    /// event can't fail the rest. One the program traps on alone is given back poisoned, for the
    /// program to refuse without running it; if even that traps, the room can't go on. One past
    /// the host's limits alone waits for the next wake, with those after it.
    fn one_by_one(&mut self, batch: Vec<Incoming>, time: f32) -> Result<(), Failure> {
        let mut rest = batch.into_iter();
        while let Some(e) = rest.next() {
            let failed = match self.alone(&e, time) {
                Ok(()) => continue,
                Err(Fail::Journal(f)) => return Err(f),
                Err(Fail::Room(f)) => f,
            };
            if !self.quarantined() {
                self.wait(std::iter::once(e).chain(rest));
                self.fail_at_limits();
            }
            return Err(failed);
        }
        Ok(())
    }

    /// One event on a fresh instance, given back poisoned if the program traps on it: a failure
    /// past the host's limits, or one that quarantines the room.
    fn alone(&mut self, e: &Incoming, time: f32) -> Result<(), Fail> {
        if self.room.is_none() {
            self.start().map_err(Fail::Room)?;
        }
        match self.call(std::slice::from_ref(e), time) {
            Err(Fail::Room(f)) if f.is_program() => {
                let Incoming::Message(c, bytes) = e else { return Ok(()) };
                self.start().map_err(Fail::Room)?;
                match self.call(&[Incoming::Message(c | POISONED, bytes.clone())], time) {
                    Err(Fail::Room(f)) if f.is_program() => {
                        self.quarantine(format!("a poisoned message traps it: {f}"));
                        Err(Fail::Room(f))
                    }
                    other => other,
                }
            }
            other => other,
        }
    }

    /// `events` wait for the next wake, before what came since.
    fn wait(&mut self, events: impl DoubleEndedIterator<Item = Incoming>) {
        for e in events.rev() {
            self.incoming.push_front(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::crc32c;

    #[test]
    fn journal_path_aliases_wait_for_the_same_room() {
        use std::sync::{Arc, mpsc};
        use std::time::Duration;

        let dir = std::env::temp_dir().join(format!("wrela-journal-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("nested")).expect("scratch directory");
        let wasm = wat::parse_str(
            r#"(module (memory (export "memory") 1)
                (func (export "serve") (param f32)))"#,
        )
        .expect("server module");
        let build = Arc::new(super::ServerBuild::from_wasm(&wasm).expect("server build"));
        let first =
            super::RoomHost::open(build.clone(), 1, &dir, "a", super::Limits::default(), None)
                .expect("first room");
        let alias = dir.join("nested/..");
        let (tx, rx) = mpsc::channel();
        let second = std::thread::spawn(move || {
            let _room =
                super::RoomHost::open(build, 1, &alias, "a", super::Limits::default(), None)
                    .expect("second room once the first closes");
            tx.send(()).expect("report open");
        });
        let early = rx.recv_timeout(Duration::from_millis(100));
        drop(first);
        if early.is_err() {
            rx.recv_timeout(Duration::from_secs(2)).expect("the journal becomes free");
        }
        second.join().expect("second room thread");
        std::fs::remove_dir_all(dir).expect("remove scratch directory");
        assert!(
            matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
            "an alias bypassed the journal lock"
        );
    }

    /// The check value every CRC-32C implementation gives.
    #[test]
    fn crc32c_is_castagnolis() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b""), 0);
    }

    #[test]
    fn crc32c_matches_the_bitwise_rule_on_seeded_bytes() {
        fn bitwise(bytes: &[u8]) -> u32 {
            let mut crc = !0u32;
            for &byte in bytes {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = if crc & 1 != 0 { (crc >> 1) ^ 0x82F6_3B78 } else { crc >> 1 };
                }
            }
            !crc
        }

        let mut seed = 0x73A9_4B21u32;
        let mut bytes = [0u8; 4096];
        for _ in 0..16 {
            for byte in &mut bytes {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *byte = seed as u8;
            }
            for len in [0, 1, 2, 3, 7, 8, 15, 16, 31, 32, 63, 64, 255, 256, 257, 1024, 4096] {
                assert_eq!(crc32c(&bytes[..len]), bitwise(&bytes[..len]), "length {len}");
            }
        }
    }
}
