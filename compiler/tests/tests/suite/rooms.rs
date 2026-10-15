//! Rooms kept durably (language.md §12, `wrela_host::rooms`): a server program on `std::net` and
//! `platform::net`, its journal on disk, and what a crash, a torn write, another build's journal,
//! a proposal that traps the room or one that runs past its deadline leave. The peers are played
//! here, in Rust, by their packed messages (`std::pack`'s rules).

use std::path::PathBuf;
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
use platform::net::serve_room
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
    WASM.get_or_init(|| {
        let root = wrela_tests::scratch_root(env!("CARGO_TARGET_TMPDIR")).join("rooms-program");
        let _ = std::fs::remove_dir_all(&root);
        let pkg = root.join("pkg");
        std::fs::create_dir_all(&pkg).expect("scratch package");
        let platform = wrela_tests::repo_root().join("platform");
        std::fs::write(
            pkg.join("wrela.toml"),
            format!(
                "[package]\nname = \"rooms\"\n\n[dependencies]\nplatform = {{ path = \"{}\" }}\n",
                platform.display()
            ),
        )
        .expect("manifest");
        std::fs::write(pkg.join("main.wrela"), PROGRAM).expect("main");
        let out = root.join("build");
        wrela_tests::must_build(&pkg, &out);
        std::fs::read(out.join("server/game.wasm")).expect("the server build")
    })
}

/// The server build, its WASM's hash, and a fresh directory for the test's rooms.
fn build(name: &str) -> (std::sync::Arc<ServerBuild>, u64, PathBuf) {
    let dir = wrela_tests::scratch_root(env!("CARGO_TARGET_TMPDIR")).join(format!("rooms-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let build = std::sync::Arc::new(ServerBuild::from_wasm(wasm()).expect("a server build"));
    (build, wrela_abi::ticks::wasm_hash(wasm()), dir)
}

fn limits() -> Limits {
    Limits { memory: 64 << 20, deadline: std::time::Duration::from_millis(500) }
}

// ---- the tests ---------------------------------------------------------------------------------

#[test]
fn what_a_room_commits_survives_a_restart() {
    let (build, hash, dir) = build("restart");
    {
        let mut room = RoomHost::open(build.clone(), hash, &dir, "a", limits()).expect("open");
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
    let mut room = RoomHost::open(build.clone(), hash, &dir, "a", limits()).expect("reopen");
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
    let mut room = RoomHost::open(build.clone(), hash, &dir, "a", limits()).expect("open");
    room.connect(1);
    room.receive(1, hello(5, 1, 0, 0));
    room.wake(0.1).expect("hello");
    room.take_outbox();
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
        let mut room = RoomHost::open(build.clone(), hash, &dir, "a", limits()).expect("open");
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
    let mut room = RoomHost::open(build.clone(), hash, &dir, "a", limits()).expect("reopen");
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
        let mut room = RoomHost::open(build.clone(), hash, &dir, "a", limits()).expect("open");
        room.connect(1);
        room.receive(1, hello(5, 1, 0, 0));
        room.wake(0.1).expect("wake");
    }
    match RoomHost::open(build.clone(), hash ^ 1, &dir, "a", limits()) {
        Err(Failure::Host(why)) => assert!(why.contains("isn't this room's journal"), "{why}"),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("another build's journal was read"),
    }
}

#[test]
fn a_proposal_that_traps_the_room_is_refused_and_the_rest_go_on() {
    let (build, hash, dir) = build("poison");
    let mut room = RoomHost::open(build.clone(), hash, &dir, "a", limits()).expect("open");
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
    let fast = Limits { memory: 64 << 20, deadline: std::time::Duration::from_millis(100) };
    let mut room = RoomHost::open(build.clone(), hash, &dir, "a", fast).expect("open");
    room.connect(1);
    room.receive(1, hello(5, 1, 0, 0));
    room.wake(0.1).expect("hello");
    room.take_outbox();
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
    let mut room = RoomHost::open(build.clone(), hash, &dir, "a", limits()).expect("open");
    room.connect(1);
    room.receive(1, hello(5, 1, 0, 0));
    room.wake(0.1).expect("hello");
    room.take_outbox();
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
        let mut room = RoomHost::open(build.clone(), hash, &dir, "a", limits()).expect("open");
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
    match RoomHost::open(build.clone(), hash, &dir, "a", limits()) {
        Err(Failure::Program(why)) => assert!(why.contains("journal record"), "{why}"),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("it opened"),
    }
}
