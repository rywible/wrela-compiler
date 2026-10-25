//! Rooms kept durably (language.md §19, `wrela_host::rooms`): a server program on `std::net` and
//! `std::net`, its journal on disk, and what a crash, a torn write, another build's journal,
//! a proposal that traps the room or one that runs past its deadline leave. The peers are played
//! here, in Rust, by their packed messages (`std::pack`'s rules).

use std::path::{Path, PathBuf};
use wrela_host::rooms::{Fault, Out, RoomHost, Status};
use wrela_host::server::{Failure, Limits, ServerBuild};

// ---- the protocol's bytes, as std::pack packs them --------------------------------------------

#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    pending: u64,
    filled: u32,
}

impl Bits {
    fn bits(&mut self, x: u64, n: u32) {
        self.pending |= (x & ((1u64 << n) - 1)) << self.filled;
        self.filled += n;
        while self.filled >= 8 {
            self.bytes.push(self.pending as u8);
            self.pending >>= 8;
            self.filled -= 8;
        }
    }
    fn varint(&mut self, mut x: u64) {
        loop {
            let group = x & 127;
            x >>= 7;
            if x == 0 {
                self.bits(group, 8);
                return;
            }
            self.bits(group | 128, 8);
        }
    }
    fn finish(mut self) -> Vec<u8> {
        if self.filled > 0 {
            self.bytes.push(self.pending as u8);
        }
        self.bytes
    }
}

struct Read<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Read<'_> {
    fn bits(&mut self, n: u32) -> u64 {
        let mut x = 0;
        for i in 0..n {
            let bit = (self.bytes[self.at / 8] >> (self.at % 8)) & 1;
            x |= u64::from(bit) << i;
            self.at += 1;
        }
        x
    }
    fn varint(&mut self) -> u64 {
        let (mut x, mut shift) = (0, 0);
        loop {
            let byte = self.bits(8);
            x |= (byte & 127) << shift;
            if byte < 128 {
                return x;
            }
            shift += 7;
        }
    }
}

pub(crate) fn hello(token: u64, session: u32, acked: u32, have: u64) -> Vec<u8> {
    let mut b = Bits::default();
    for v in [0, token, u64::from(session), u64::from(acked), have] {
        b.varint(v);
    }
    b.finish()
}

fn ack(acked: u32) -> Vec<u8> {
    let mut b = Bits::default();
    b.varint(2);
    b.varint(u64::from(acked));
    b.finish()
}

pub(crate) fn propose(peer: u32, session: u32, counter: u32, value: u32) -> Vec<u8> {
    let mut b = Bits::default();
    for v in [1, peer, session, counter, value] {
        b.varint(u64::from(v));
    }
    b.finish()
}

/// What a room sends, as these tests read it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Down {
    Welcome { peer: u32, seq: u64 },
    State { seq: u64, tags: Vec<u32> },
    Committed { seq: u64, counter: u32, value: u32 },
    Rejected { counter: u32, reason: u32 },
    Done { counter: u32 },
    Refused { why: u32 },
}

pub(crate) fn down(bytes: &[u8]) -> Down {
    let mut r = Read { bytes, at: 0 };
    match r.varint() {
        0 => {
            let peer = r.varint() as u32;
            let _session = r.varint();
            Down::Welcome { peer, seq: r.varint() }
        }
        1 => {
            let seq = r.varint();
            let n = r.varint();
            Down::State { seq, tags: (0..n).map(|_| r.varint() as u32).collect() }
        }
        2 => {
            let seq = r.varint();
            let (_peer, _session, counter) = (r.varint(), r.varint(), r.varint() as u32);
            let _at = r.varint();
            Down::Committed { seq, counter, value: r.varint() as u32 }
        }
        3 => Down::Rejected { counter: r.varint() as u32, reason: r.varint() as u32 },
        4 => Down::Done { counter: r.varint() as u32 },
        5 => Down::Refused { why: r.varint() as u32 },
        k => panic!("no message {k}"),
    }
}

/// A refusal's bytes: `Down::Refused { why }`.
fn refused(why: u32) -> Vec<u8> {
    let mut b = Bits::default();
    b.varint(5);
    b.varint(u64::from(why));
    b.finish()
}

/// What a room released to connection `conn`, read.
fn sent_to(out: &[Out], conn: u32) -> Vec<Down> {
    out.iter()
        .filter_map(|o| match o {
            Out::Send(c, b) if *c == conn => Some(down(b)),
            _ => None,
        })
        .collect()
}

// ---- the server program ------------------------------------------------------------------------

/// A room of tags. A multiple of 7 is refused; 13 traps the reducer, and 99 loops in it.
const PROGRAM: &str = r#"
use std::net::serve_room
use std::net::{Entry, Reduce, Room, Verdict}
use std::pack::Pack

struct Tags: Clone + Pack {
    all: Vec<u32>,
}

impl Reduce<u32> for Tags {
    fn check(self, entry: Entry<u32>) -> Verdict {
        if entry.value == 13 {
            panic("thirteen")
        }
        if entry.value == 99 {
            loop {}
        }
        if entry.value % 7 == 0 { Verdict::Reject(7) } else { Verdict::Accept }
    }

    fn apply(mut self, entry: Entry<u32>) {
        self.all.push(entry.value)
    }
}

pub fn serve_init() -> Room<Tags, u32> {
    Room::new(Tags { all: Vec::new() })
}

pub fn serve(room: mut Room<Tags, u32>, time: f32) {
    serve_room(mut room, u64(time * 1000.0))
}
"#;

/// The server program's module, built once for every test here.
pub(crate) fn wasm() -> &'static [u8] {
    static WASM: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    WASM.get_or_init(|| server_wasm("rooms-program", PROGRAM))
}

/// A server program that never reads its events.
pub(crate) const DEAF: &str = r#"
pub fn serve(time: f32) {}
"#;

/// The server build of `program` (a package's main.wrela), in scratch directory `name`.
pub(crate) fn server_wasm(name: &str, program: &str) -> Vec<u8> {
    let root = super::scratch(name);
    let pkg = root.join("pkg");
    super::write_package(&pkg, "rooms", program);
    let out = root.join("build");
    wrela_tests::must_build(&pkg, &out);
    std::fs::read(out.join("server/game.wasm")).expect("the server build")
}

/// The server build, its WASM's hash, and a fresh directory for the test's rooms.
fn build(name: &str) -> (std::sync::Arc<ServerBuild>, u64, PathBuf) {
    let dir = crate::scratch_root().join(format!("rooms-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let build = std::sync::Arc::new(ServerBuild::from_wasm(wasm()).expect("a server build"));
    (build, wrela_abi::ticks::wasm_hash(wasm()), dir)
}

/// The limits the rooms' tests open them with: 64 MiB, and half a second a call.
pub(crate) fn limits() -> Limits {
    Limits {
        memory: 64 << 20,
        deadline: std::time::Duration::from_millis(500),
        ..Limits::default()
    }
}

/// Room "a" of `build` in `dir`, opened with `limits` and no services.
fn open(
    build: &std::sync::Arc<ServerBuild>,
    hash: u64,
    dir: &Path,
    limits: Limits,
) -> Result<RoomHost, Failure> {
    RoomHost::open(build.clone(), hash, dir, "a", limits, None)
}

/// Room "a" of `build`, opened with `limits`, with peer 1 connected and greeted: what it sent
/// taken.
fn greeted(build: &std::sync::Arc<ServerBuild>, hash: u64, dir: &Path, limits: Limits) -> RoomHost {
    let mut room = open(build, hash, dir, limits).expect("open");
    room.connect(1);
    room.receive(1, hello(5, 1, 0, 0));
    room.wake(0.1).expect("hello");
    room.take_outbox();
    room
}

// ---- the tests ---------------------------------------------------------------------------------

#[test]
fn what_a_room_commits_survives_a_restart() {
    let (build, hash, dir) = build("restart");
    {
        let mut room = open(&build, hash, &dir, limits()).expect("open");
        room.connect(1);
        room.receive(1, hello(5, 1, 0, 0));
        room.receive(1, propose(1, 1, 1, 3));
        room.receive(1, propose(1, 1, 2, 7));
        room.receive(1, propose(1, 1, 3, 4));
        room.wake(0.1).expect("wake");
        let got = sent_to(&room.take_outbox(), 1);
        assert_eq!(
            got,
            [
                Down::Welcome { peer: 1, seq: 0 },
                Down::Committed { seq: 1, counter: 1, value: 3 },
                Down::Rejected { counter: 2, reason: 7 },
                Down::Committed { seq: 2, counter: 3, value: 4 },
            ]
        );
    }
    // A new host process: the journal is read back first, and a peer that has nothing gets
    // the log; the proposals go on from where they were.
    let mut room = open(&build, hash, &dir, limits()).expect("reopen");
    room.connect(2);
    room.receive(2, hello(5, 1, 3, 0));
    room.receive(2, propose(1, 1, 4, 8));
    room.wake(0.2).expect("wake");
    let got = sent_to(&room.take_outbox(), 2);
    assert_eq!(
        got,
        [
            Down::Welcome { peer: 1, seq: 2 },
            Down::Committed { seq: 1, counter: 1, value: 3 },
            Down::Committed { seq: 2, counter: 3, value: 4 },
            Down::Committed { seq: 3, counter: 4, value: 8 },
        ]
    );
}

#[test]
fn nothing_is_sent_before_its_records_are_kept() {
    let (build, hash, dir) = build("unkept");
    let mut room = greeted(&build, hash, &dir, limits());
    // The journal fails to keep the commit: nothing is sent, the connection closes, and the
    // room starts again from what it kept.
    room.inject(Fault::Append);
    room.receive(1, propose(1, 1, 1, 3));
    assert!(matches!(room.wake(0.2), Err(Failure::Host(_))));
    assert_eq!(room.take_outbox(), [Out::Close(1)]);
    // The peer comes back and sends its proposal again, with its counter: it's committed once.
    room.connect(2);
    room.receive(2, hello(5, 1, 0, 0));
    room.receive(2, propose(1, 1, 1, 3));
    room.wake(0.3).expect("wake");
    let got = sent_to(&room.take_outbox(), 2);
    assert_eq!(
        got,
        [Down::Welcome { peer: 1, seq: 0 }, Down::Committed { seq: 1, counter: 1, value: 3 }]
    );
    // Joined, its session and one commit.
    assert_eq!(room.journal().expect("journal").len(), 3);
}

#[test]
fn a_torn_last_record_is_cut_from_the_journal() {
    let (build, hash, dir) = build("torn");
    {
        let mut room = open(&build, hash, &dir, limits()).expect("open");
        room.connect(1);
        room.receive(1, hello(5, 1, 0, 0));
        room.receive(1, propose(1, 1, 1, 3));
        room.receive(1, propose(1, 1, 2, 4));
        room.wake(0.1).expect("wake");
    }
    // A crash in the middle of the last write: its last three bytes never reached the disk.
    let path = dir.join("a.journal");
    let len = std::fs::metadata(&path).expect("journal").len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open")
        .set_len(len - 3)
        .expect("cut");
    let mut room = open(&build, hash, &dir, limits()).expect("reopen");
    assert_eq!(room.journal().expect("journal").len(), 3, "the torn commit is gone");
    room.connect(2);
    room.receive(2, hello(9, 1, 0, 0));
    room.wake(0.2).expect("wake");
    let got = sent_to(&room.take_outbox(), 2);
    assert_eq!(
        got,
        [Down::Welcome { peer: 2, seq: 1 }, Down::Committed { seq: 1, counter: 1, value: 3 }]
    );
}

#[test]
fn another_builds_journal_isnt_read() {
    let (build, hash, dir) = build("other");
    {
        let mut room = open(&build, hash, &dir, limits()).expect("open");
        room.connect(1);
        room.receive(1, hello(5, 1, 0, 0));
        room.wake(0.1).expect("wake");
    }
    match open(&build, hash ^ 1, &dir, limits()) {
        Err(Failure::Host(why)) => assert!(why.contains("isn't this room's journal"), "{why}"),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("another build's journal was read"),
    }
}

#[test]
fn a_proposal_that_traps_the_room_is_refused_and_the_rest_go_on() {
    let (build, hash, dir) = build("poison");
    let mut room = open(&build, hash, &dir, limits()).expect("open");
    room.connect(1);
    room.receive(1, hello(5, 1, 0, 0));
    room.receive(1, propose(1, 1, 1, 3));
    room.receive(1, propose(1, 1, 2, 13));
    room.receive(1, propose(1, 1, 3, 4));
    room.wake(0.1).expect("isolated");
    let got = sent_to(&room.take_outbox(), 1);
    let trapped = u32::MAX;
    assert_eq!(
        got,
        [
            Down::Welcome { peer: 1, seq: 0 },
            Down::Committed { seq: 1, counter: 1, value: 3 },
            Down::Rejected { counter: 2, reason: trapped },
            Down::Committed { seq: 2, counter: 3, value: 4 },
        ]
    );
    // Sent again, it's refused from what was kept, without running it.
    room.receive(1, propose(1, 1, 2, 13));
    room.wake(0.2).expect("again");
    assert_eq!(sent_to(&room.take_outbox(), 1), [Down::Rejected { counter: 2, reason: trapped }]);
    assert_eq!(room.status(), &Status::Live);
}

#[test]
fn a_proposal_past_its_deadline_keeps_nothing() {
    let (build, hash, dir) = build("deadline");
    let fast = Limits {
        memory: 64 << 20,
        deadline: std::time::Duration::from_millis(100),
        ..Limits::default()
    };
    let mut room = greeted(&build, hash, &dir, fast);
    let before = room.journal().expect("journal").len();
    room.receive(1, propose(1, 1, 1, 99));
    // The host's limit, not the program's: nothing is kept or refused, and the event waits;
    // after the retries the peer is let go, to come back later.
    for _ in 0..3 {
        assert_eq!(room.wake(0.2), Err(Failure::Deadline));
    }
    assert_eq!(room.take_outbox(), [Out::Close(1)]);
    assert_eq!(room.journal().expect("journal").len(), before, "no refusal was kept");
}

#[test]
fn a_compaction_that_fails_leaves_the_old_journal() {
    let (build, hash, dir) = build("compact");
    let mut room = greeted(&build, hash, &dir, limits());
    // Past `COMPACT_AFTER` (1024) entries the room compacts; the first try fails before its
    // new journal is renamed into place.
    room.inject(Fault::Compact);
    for c in 1..=1030u32 {
        room.receive(1, propose(1, 1, c, c * 7 + 3));
        // A peer acknowledges as it goes, so its window moves on.
        if c % 100 == 0 {
            room.receive(1, ack(c));
        }
    }
    let woke = room.wake(0.2);
    assert!(matches!(woke, Err(Failure::Host(_))), "{woke:?}");
    let old = room.journal().expect("journal");
    assert!(old.len() > 1000, "the old journal is whole: {} records", old.len());
    // The peer comes back; the room compacts this time, and has every entry.
    room.connect(2);
    room.receive(2, hello(5, 1, 0, 0));
    room.wake(0.3).expect("wake");
    room.take_outbox();
    room.receive(2, propose(1, 1, 1031, 2));
    room.wake(0.4).expect("wake");
    let after = room.journal().expect("journal");
    assert!(after.len() < 10, "compacted to a snapshot: {} records", after.len());
    room.connect(3);
    room.receive(3, hello(77, 1, 0, 0));
    room.wake(0.5).expect("wake");
    match &sent_to(&room.take_outbox(), 3)[1] {
        Down::State { seq, tags } => assert!(*seq > 1024 && tags.len() as u64 == *seq),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_journal_its_program_cant_read_quarantines_the_room() {
    let (build, hash, dir) = build("quarantine");
    {
        let mut room = open(&build, hash, &dir, limits()).expect("open");
        room.connect(1);
        room.receive(1, hello(5, 1, 0, 0));
        room.wake(0.1).expect("wake");
    }
    // A record whose checksum is right but whose bytes aren't a record of this program.
    let path = dir.join("a.journal");
    let mut bytes = std::fs::read(&path).expect("journal");
    let record = [255u8, 255, 255];
    bytes.extend_from_slice(&(record.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&wrela_host::rooms::crc32c(&record).to_le_bytes());
    bytes.extend_from_slice(&record);
    std::fs::write(&path, bytes).expect("write");
    match open(&build, hash, &dir, limits()) {
        Err(Failure::Program(why)) => assert!(why.contains("journal record"), "{why}"),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("it opened"),
    }
}

/// `n` proposals of peer 1 from connection `conn`, from counter `from`, acknowledging as they go
/// so its window moves on.
fn proposals(room: &mut RoomHost, conn: u32, from: u32, n: u32) {
    for c in from..from + n {
        room.receive(conn, propose(1, 1, c, c * 7 + 3));
        if c % 100 == 0 {
            room.receive(conn, ack(c));
        }
    }
}

/// How many entries room `dir`/`a` has, read back by a new host process.
fn entries_after_a_restart(build: &std::sync::Arc<ServerBuild>, hash: u64, dir: &Path) -> u64 {
    let mut room = open(build, hash, dir, roomy()).expect("reopen");
    room.connect(9);
    room.receive(9, hello(77, 1, 0, 0));
    room.wake(9.0).expect("wake");
    match sent_to(&room.take_outbox(), 9).first() {
        Some(Down::Welcome { seq, .. }) => *seq,
        other => panic!("{other:?}"),
    }
}

/// Limits for a call that takes a hundred thousand proposals.
fn roomy() -> Limits {
    Limits { deadline: std::time::Duration::from_secs(20), commands: 1 << 30, ..limits() }
}

#[test]
fn a_journal_compacted_while_its_read_back_keeps_what_it_hadnt_read() {
    let (build, hash, dir) = build("long-journal");
    let mut room = open(&build, hash, &dir, roomy()).expect("open");
    room.connect(1);
    room.receive(1, hello(5, 1, 0, 0));
    room.wake(0.1).expect("hello");
    room.take_outbox();
    // A call's worth of proposals (a megabyte of them), whose compaction fails: the journal
    // keeps every record, more than a call reads back.
    room.inject(Fault::Compact);
    proposals(&mut room, 1, 1, 77_600);
    assert!(matches!(room.wake(0.2), Err(Failure::Host(_))));
    let long = room.journal().expect("journal");
    let bytes: usize = long.iter().map(Vec::len).sum();
    assert!(bytes > wrela_abi::input::PAYLOAD_PER_CALL, "{bytes} bytes");
    // The room starts again: it compacts after the first call's worth it reads back, and the
    // records after that stay in the journal behind the snapshot.
    room.connect(2);
    room.receive(2, hello(5, 1, 0, 0));
    room.wake(0.3).expect("wake");
    let seq = match sent_to(&room.take_outbox(), 2).first() {
        Some(Down::Welcome { seq, .. }) => *seq,
        other => panic!("{other:?}"),
    };
    assert_eq!(seq, 77_600);
    assert!(room.journal().expect("journal").len() < long.len(), "it compacted");
    drop(room);
    assert_eq!(entries_after_a_restart(&build, hash, &dir), seq);
    assert_eq!(entries_after_a_restart(&build, hash, &dir), seq);
}

#[test]
fn a_wake_past_a_calls_bytes_gives_the_room_all_of_it() {
    let (build, hash, dir) = build("long-wake");
    let mut room = open(&build, hash, &dir, limits()).expect("open");
    room.connect(1);
    room.connect(2);
    room.receive(1, hello(5, 1, 0, 0));
    // Twenty messages of 60 KiB, more than a call is given, before the proposal.
    for _ in 0..20 {
        room.receive(2, vec![255; 60 << 10]);
    }
    room.receive(1, propose(1, 1, 1, 3));
    room.wake(0.1).expect("wake");
    let got = sent_to(&room.take_outbox(), 1);
    assert_eq!(
        got,
        [Down::Welcome { peer: 1, seq: 0 }, Down::Committed { seq: 1, counter: 1, value: 3 }]
    );
}

#[test]
fn a_journal_renamed_but_not_reopened_is_reopened() {
    let (build, hash, dir) = build("renamed");
    let mut room = greeted(&build, hash, &dir, limits());
    // The compaction's new journal is renamed into place, then the host fails before it holds
    // the new file: the room starts again, and what it keeps from then on is on the disk.
    room.inject(Fault::AfterRename);
    proposals(&mut room, 1, 1, 1030);
    assert!(matches!(room.wake(0.2), Err(Failure::Host(_))));
    room.connect(2);
    room.receive(2, hello(5, 1, 0, 0));
    room.receive(2, propose(1, 1, 1031, 2));
    room.wake(0.3).expect("wake");
    let seq = match sent_to(&room.take_outbox(), 2).last() {
        Some(Down::Committed { seq, counter: 1031, .. }) => *seq,
        other => panic!("{other:?}"),
    };
    drop(room);
    assert_eq!(entries_after_a_restart(&build, hash, &dir), seq);
}

#[test]
fn a_connection_is_its_peers_only_through_its_hello() {
    let (build, hash, dir) = build("tags");
    let mut room = greeted(&build, hash, &dir, limits());
    // Another connection names peer 1, whose id every entry shows, without its token: refused,
    // and the peer keeps its connection.
    room.connect(2);
    room.receive(2, propose(1, 1, 1, 3));
    room.wake(0.2).expect("stranger");
    assert_eq!(room.take_outbox(), [Out::Send(2, refused(2)), Out::Close(2)]);
    // A proposal that traps the room, in a later call than the hello: the fresh instance
    // knows connection 1 is peer 1's by its tag, and refuses the proposal durably.
    room.receive(1, propose(1, 1, 1, 13));
    room.receive(1, propose(1, 1, 2, 4));
    room.wake(0.3).expect("isolated");
    assert_eq!(
        sent_to(&room.take_outbox(), 1),
        [
            Down::Rejected { counter: 1, reason: u32::MAX },
            Down::Committed { seq: 1, counter: 2, value: 4 },
        ]
    );
}

#[test]
fn a_server_that_doesnt_read_its_events_is_quarantined() {
    let wasm = server_wasm("deaf-program", DEAF);
    let dir = crate::scratch_root().join("rooms-deaf");
    let _ = std::fs::remove_dir_all(&dir);
    let build = std::sync::Arc::new(ServerBuild::from_wasm(&wasm).expect("a server build"));
    let hash = wrela_abi::ticks::wasm_hash(&wasm);
    let mut room = open(&build, hash, &dir, limits()).expect("open");
    room.connect(1);
    room.receive(1, hello(5, 1, 0, 0));
    assert!(matches!(room.wake(0.1), Err(Failure::Program(_))));
    assert!(matches!(room.status(), Status::Quarantined(_)), "{:?}", room.status());
    // Its connections close, and it takes no more: what comes goes nowhere.
    assert_eq!(room.take_outbox(), [Out::Close(1)]);
    room.connect(2);
    room.receive(2, hello(6, 1, 0, 0));
    assert_eq!(room.take_outbox(), [Out::Close(2)]);
    assert_eq!(room.waiting(), 0);
}

#[test]
fn an_event_past_the_hosts_limits_while_isolating_waits_with_those_after_it() {
    let (build, hash, dir) = build("isolate-limits");
    let fast = Limits { deadline: std::time::Duration::from_millis(100), ..limits() };
    let mut room = greeted(&build, hash, &dir, fast);
    // 13 traps the room, so the call is taken one event at a time: 13 is refused, then 99 runs
    // past its deadline, and it and 4 wait.
    room.receive(1, propose(1, 1, 1, 13));
    room.receive(1, propose(1, 1, 2, 99));
    room.receive(1, propose(1, 1, 3, 4));
    assert_eq!(room.wake(0.2), Err(Failure::Deadline));
    assert_eq!(sent_to(&room.take_outbox(), 1), [Down::Rejected { counter: 1, reason: u32::MAX }]);
    assert_eq!(room.waiting(), 2);
    // They're tried again, and after the retries the peer is let go, to send them again later.
    assert_eq!(room.wake(0.3), Err(Failure::Deadline));
    assert_eq!(room.wake(0.4), Err(Failure::Deadline));
    assert_eq!(room.take_outbox(), [Out::Close(1)]);
    assert_eq!(room.waiting(), 0);
}

#[test]
fn a_call_past_the_command_cap_is_taken_one_event_at_a_time() {
    let (build, hash, dir) = build("command-cap");
    let capped = Limits { commands: 8 << 10, ..limits() };
    let mut room = open(&build, hash, &dir, capped).expect("open");
    room.connect(1);
    room.receive(1, hello(5, 1, 0, 0));
    room.wake(0.1).expect("hello");
    proposals(&mut room, 1, 1, 50);
    room.wake(0.2).expect("50");
    proposals(&mut room, 1, 51, 50);
    room.wake(0.3).expect("100");
    room.take_outbox();
    // Twenty peers join at once: each is sent the hundred entries, more together than one
    // call's commands may hold, but each alone fits.
    for c in 2..22 {
        room.connect(c);
        room.receive(c, hello(100 + u64::from(c), 1, 0, 0));
    }
    room.wake(0.4).expect("one at a time");
    let out = room.take_outbox();
    for c in 2..22 {
        let got = sent_to(&out, c);
        assert_eq!(got.len(), 101, "connection {c}");
        assert!(matches!(got[0], Down::Welcome { seq: 100, .. }), "{:?}", got[0]);
    }
    assert!(!out.contains(&Out::Close(1)));
    assert_eq!(room.status(), &Status::Live);
}

#[test]
fn one_room_host_at_a_time_holds_a_journal() {
    let (build, hash, dir) = build("held");
    let mut first = open(&build, hash, &dir, limits()).expect("open");
    first.connect(1);
    first.receive(1, hello(5, 1, 0, 0));
    first.receive(1, propose(1, 1, 1, 3));
    first.wake(0.1).expect("wake");
    // Another host of the same room waits for the first to close, then reads what it kept.
    let (b, d) = (build.clone(), dir.clone());
    let second = std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let mut room = open(&b, hash, &d, limits()).expect("opens once free");
        room.connect(2);
        room.receive(2, hello(5, 2, 1, 0));
        room.wake(0.2).expect("wake");
        (started.elapsed(), sent_to(&room.take_outbox(), 2))
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    drop(first);
    let (waited, got) = second.join().expect("the second host");
    assert!(waited >= std::time::Duration::from_millis(250), "it waited {waited:?}");
    assert_eq!(
        got,
        [Down::Welcome { peer: 1, seq: 1 }, Down::Committed { seq: 1, counter: 1, value: 3 }]
    );
}

#[test]
fn a_room_that_keeps_nothing_leaves_no_journal() {
    let (build, hash, dir) = build("nothing");
    let mut room = open(&build, hash, &dir, limits()).expect("open");
    room.connect(1);
    room.receive(1, vec![255; 3]);
    room.wake(0.1).expect("wake");
    drop(room);
    assert!(!dir.join("a.journal").exists());
}
