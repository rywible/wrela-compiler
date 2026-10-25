//! # The tick log, version 1
//!
//! A program's ticks as a host ran them (language.md's `std::tick`): the build, the first
//! world's state hash, then each tick's records and the state hash its step gave. Given the
//! build, the first world and the records, every tick's hash is fixed, so another host replays
//! the log and checks each hash (`wrela-host --replay`). Both hosts write one in test mode and
//! on request. Everything is little-endian.
//!
//! | Offset | Field |
//! |---|---|
//! | 0 | magic: the bytes `WRTL` |
//! | 4 | version: [`VERSION`]; a reader rejects any other |
//! | 8 | the build's WASM hash: FNV-1a 64 of its `.wasm` file's bytes ([`wasm_hash`]) |
//! | 16 | `hz`: the ticker's rate |
//! | 20 | 0 |
//! | 24 | the first world's state hash: the ticker's state after `init`, as its `hash` gave it |
//! | 32 | the ticks, from tick 0, one after another: each `tick` (a `u32`, its number), `count` (a `u32`), `count` records of [`crate::input::EVENT_SIZE`] bytes, then the state hash after its step (a `u64`) |
//!
//! A tick's records are the input events the host stamped at its start (at most
//! [`crate::memory::MAX_TICK_RECORDS`]), in the layout `wrela.input` gives them.
//!
//! ## Test mode's schedule
//!
//! A host in test mode runs frame `i` at [`frame_time`], and in lockstep (#43 §2.3) runs the
//! ticks before it first ([`lockstep_ticks`]). Both hosts compute these the same way: the
//! vectors check them.

use crate::hash::StateHash;
use crate::input::EVENT_SIZE;
use crate::memory::MAX_TICK_RECORDS;
use std::fmt;

pub const VERSION: u32 = 1;
pub const MAGIC: [u8; 4] = *b"WRTL";
/// The header's length: everything before the first tick.
pub const HEADER_LEN: usize = 32;

/// A tick log, read or to be written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TickLog {
    /// FNV-1a 64 of the build's WASM ([`wasm_hash`]).
    pub wasm_hash: u64,
    pub hz: u32,
    /// The first world's state hash: after `init`, before tick 0.
    pub first: u64,
    /// Every tick, from tick 0 in order.
    pub ticks: Vec<Tick>,
}

/// One tick: its records, and the state hash its step gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tick {
    pub records: Vec<[u8; EVENT_SIZE as usize]>,
    pub hash: u64,
}

/// What's wrong with a tick log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TickLogError {
    TooShort,
    BadMagic,
    WrongVersion(u32),
    /// Tick `at` (its place in the log) has the number `tick`.
    OutOfOrder {
        at: u32,
        tick: u32,
    },
    /// Tick `at` has more records than one tick can consume.
    TooManyRecords {
        at: u32,
        count: u32,
    },
    /// The log ends inside tick `at`.
    Truncated {
        at: u32,
    },
}

impl fmt::Display for TickLogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TickLogError::TooShort => write!(f, "too short for a tick log's header"),
            TickLogError::BadMagic => write!(f, "not a tick log: its magic isn't `WRTL`"),
            TickLogError::WrongVersion(v) => {
                write!(f, "tick log version {v}, but this host reads version {VERSION}")
            }
            TickLogError::OutOfOrder { at, tick } => {
                write!(f, "the log's tick {at} is numbered {tick}: ticks go from 0 in order")
            }
            TickLogError::TooManyRecords { at, count } => write!(
                f,
                "the log's tick {at} has {count} records; a tick takes at most {MAX_TICK_RECORDS}"
            ),
            TickLogError::Truncated { at } => write!(f, "the log ends inside tick {at}"),
        }
    }
}

impl std::error::Error for TickLogError {}

/// FNV-1a 64 of a build's WASM: which build a log is for.
pub fn wasm_hash(wasm: &[u8]) -> u64 {
    let mut hash = StateHash::new();
    hash.update(wasm);
    hash.value()
}

/// The time of frame `i` at `fps` frames a second: `i / fps` in f64, rounded to f32.
pub fn frame_time(i: u32, fps: f64) -> f32 {
    (f64::from(i) / fps) as f32
}

/// How many ticks have run before frame `i` in lockstep: ⌊(i + 1) · hz / fps⌋, in f64. Frame `i`
/// waits for them, and the ticker waits for frame `i` before the next, so the frame draws the
/// same snapshot in both hosts.
pub fn lockstep_ticks(i: u32, hz: u32, fps: f64) -> u32 {
    ((f64::from(i) + 1.0) * f64::from(hz) / fps).floor() as u32
}

/// What a build hands the build that replaces it while it runs (hot reload): the ticks to run
/// again, each with its records, by tick (those it ran, and those it was still to replay from
/// the build before it and hadn't reached), and how many ticks there were: its next, or as far
/// as its replay went, whichever is further. Both hosts carry ticks over by this: the vectors
/// check it.
pub fn carried<R: Clone>(
    ran: &[(u32, R)],
    replay: Option<(u32, &[(u32, R)])>,
    next: u32,
) -> (u32, Vec<(u32, R)>) {
    let mut ticks = ran.to_vec();
    if let Some((_, pending)) = replay {
        // A set of the ticks run: a session's reloads each carry all its ticks so far.
        let run: std::collections::HashSet<u32> = ran.iter().map(|(t, _)| *t).collect();
        ticks.extend(pending.iter().filter(|(k, _)| !run.contains(k)).cloned());
    }
    ticks.sort_by_key(|t| t.0);
    (next.max(replay.map_or(0, |r| r.0)), ticks)
}

impl TickLog {
    /// A log of no ticks yet.
    pub fn new(wasm_hash: u64, hz: u32, first: u64) -> TickLog {
        TickLog { wasm_hash, hz, first, ticks: Vec::new() }
    }

    /// Its bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.ticks.len() * 16);
        out.extend(MAGIC);
        out.extend(VERSION.to_le_bytes());
        out.extend(self.wasm_hash.to_le_bytes());
        out.extend(self.hz.to_le_bytes());
        out.extend(0u32.to_le_bytes());
        out.extend(self.first.to_le_bytes());
        for (k, t) in self.ticks.iter().enumerate() {
            out.extend((k as u32).to_le_bytes());
            out.extend((t.records.len() as u32).to_le_bytes());
            for r in &t.records {
                out.extend(r);
            }
            out.extend(t.hash.to_le_bytes());
        }
        out
    }

    /// Reads a log's bytes.
    pub fn decode(bytes: &[u8]) -> Result<TickLog, TickLogError> {
        if bytes.len() < HEADER_LEN {
            return Err(TickLogError::TooShort);
        }
        if bytes[..4] != MAGIC {
            return Err(TickLogError::BadMagic);
        }
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4"));
        let u64_at = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8"));
        let version = u32_at(4);
        if version != VERSION {
            return Err(TickLogError::WrongVersion(version));
        }
        let mut log = TickLog::new(u64_at(8), u32_at(16), u64_at(24));
        let mut at = HEADER_LEN;
        let event = EVENT_SIZE as usize;
        while at < bytes.len() {
            let k = log.ticks.len() as u32;
            let truncated = TickLogError::Truncated { at: k };
            if bytes.len() - at < 8 {
                return Err(truncated);
            }
            let (tick, count) = (u32_at(at), u32_at(at + 4));
            if tick != k {
                return Err(TickLogError::OutOfOrder { at: k, tick });
            }
            if count > MAX_TICK_RECORDS {
                return Err(TickLogError::TooManyRecords { at: k, count });
            }
            let count = count as usize;
            at += 8;
            let need =
                count.checked_mul(event).and_then(|n| n.checked_add(8)).ok_or(truncated.clone())?;
            if bytes.len() - at < need {
                return Err(truncated);
            }
            let records = bytes[at..at + count * event]
                .chunks_exact(event)
                .map(|c| c.try_into().expect("an event's bytes"))
                .collect();
            at += count * event;
            log.ticks.push(Tick { records, hash: u64_at(at) });
            at += 8;
        }
        Ok(log)
    }
}

/// How two tick logs compare: two builds' runs of one input (G-sim). Unlike a replay it doesn't
/// look at their WASM hashes, which differ when the builds do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TickComparison {
    /// Every tick's state hash is the same, as are the first world's.
    Equal { ticks: usize },
    /// The ticks run at different rates.
    Rate { a: u32, b: u32 },
    /// The first worlds (after `init`) differ.
    First { a: u64, b: u64 },
    /// Tick `tick`'s records differ: the runs didn't have the same input.
    Records { tick: usize },
    /// Tick `tick`'s state hash differs: the first that does.
    Hash { tick: usize, a: u64, b: u64 },
    /// One log is longer: the `equal` ticks both have agree.
    Length { equal: usize, a: usize, b: usize },
}

impl TickComparison {
    pub fn of(a: &TickLog, b: &TickLog) -> TickComparison {
        if a.hz != b.hz {
            return TickComparison::Rate { a: a.hz, b: b.hz };
        }
        if a.first != b.first {
            return TickComparison::First { a: a.first, b: b.first };
        }
        for (tick, (x, y)) in a.ticks.iter().zip(&b.ticks).enumerate() {
            if x.records != y.records {
                return TickComparison::Records { tick };
            }
            if x.hash != y.hash {
                return TickComparison::Hash { tick, a: x.hash, b: y.hash };
            }
        }
        if a.ticks.len() != b.ticks.len() {
            let equal = a.ticks.len().min(b.ticks.len());
            return TickComparison::Length { equal, a: a.ticks.len(), b: b.ticks.len() };
        }
        TickComparison::Equal { ticks: a.ticks.len() }
    }
}

impl fmt::Display for TickComparison {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = crate::hash::hex;
        match *self {
            TickComparison::Equal { ticks } => {
                write!(f, "equal: {ticks} ticks, each state hash the same")
            }
            TickComparison::Rate { a, b } => write!(f, "the ticks run at {a} Hz and {b} Hz"),
            TickComparison::First { a, b } => {
                write!(f, "the first worlds differ: {} and {}", hex(a), hex(b))
            }
            TickComparison::Records { tick } => {
                write!(f, "tick {tick}'s records differ: the runs didn't have the same input")
            }
            TickComparison::Hash { tick, a, b } => {
                write!(
                    f,
                    "tick {tick} is the first whose state hash differs: {} and {}",
                    hex(a),
                    hex(b)
                )
            }
            TickComparison::Length { equal, a, b } => {
                write!(f, "the first {equal} ticks are equal, and the logs have {a} and {b} ticks")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Event;

    fn sample() -> TickLog {
        let mut log = TickLog::new(0x0123_4567_89ab_cdef, 60, 0xdead_beef_0000_0001);
        log.ticks.push(Tick { records: Vec::new(), hash: 7 });
        log.ticks.push(Tick {
            records: vec![Event::key(true, 42, false, 0).bytes(), Event::text('a').bytes()],
            hash: 0xffff_0000_ffff_0000,
        });
        log
    }

    /// Two builds' logs compare by state hash, not WASM hash: the first tick that differs.
    #[test]
    fn two_logs_compare_by_their_first_differing_tick() {
        let a = sample();
        let mut b = a.clone();
        b.wasm_hash = 1;
        assert_eq!(TickComparison::of(&a, &b), TickComparison::Equal { ticks: 2 });
        b.ticks[1].hash = 9;
        assert_eq!(
            TickComparison::of(&a, &b),
            TickComparison::Hash { tick: 1, a: 0xffff_0000_ffff_0000, b: 9 }
        );
        assert!(TickComparison::of(&a, &b).to_string().starts_with("tick 1 is the first"));
        let mut c = a.clone();
        c.ticks[0].records.push(Event::text('b').bytes());
        assert_eq!(TickComparison::of(&a, &c), TickComparison::Records { tick: 0 });
        let mut d = a.clone();
        d.ticks.pop();
        assert_eq!(TickComparison::of(&a, &d), TickComparison::Length { equal: 1, a: 2, b: 1 });
        let mut e = a.clone();
        e.first = 0;
        assert!(matches!(TickComparison::of(&a, &e), TickComparison::First { .. }));
        let mut f = a.clone();
        f.hz = 30;
        assert_eq!(TickComparison::of(&a, &f), TickComparison::Rate { a: 60, b: 30 });
    }

    /// The format is part of the contract: golden bytes.
    #[test]
    fn golden_bytes() {
        let bytes = sample().encode();
        assert_eq!(&bytes[..4], b"WRTL");
        assert_eq!(bytes.len(), HEADER_LEN + (8 + 8) + (8 + 2 * 24 + 8));
        let hex: String = bytes[..32].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "5752544c01000000efcdab89674523013c0000000000000001000000efbeadde");
        assert_eq!(&bytes[32..40], &[0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&bytes[40..48], &7u64.to_le_bytes());
        assert_eq!(&bytes[48..56], &[1, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(TickLog::decode(&bytes).expect("reads back"), sample());
    }

    #[test]
    fn bad_logs_say_why() {
        let good = sample().encode();
        assert_eq!(TickLog::decode(&good[..10]), Err(TickLogError::TooShort));
        let mut b = good.clone();
        b[0] = b'X';
        assert_eq!(TickLog::decode(&b), Err(TickLogError::BadMagic));
        let mut b = good.clone();
        b[4] = 9;
        assert_eq!(TickLog::decode(&b), Err(TickLogError::WrongVersion(9)));
        let mut b = good.clone();
        b[48] = 5;
        assert_eq!(TickLog::decode(&b), Err(TickLogError::OutOfOrder { at: 1, tick: 5 }));
        assert_eq!(
            TickLog::decode(&good[..good.len() - 3]),
            Err(TickLogError::Truncated { at: 1 })
        );
        let mut oversized = TickLog::new(1, 60, 2);
        oversized.ticks.push(Tick {
            records: vec![Event::text('x').bytes(); MAX_TICK_RECORDS as usize + 1],
            hash: 3,
        });
        assert_eq!(
            TickLog::decode(&oversized.encode()),
            Err(TickLogError::TooManyRecords { at: 0, count: MAX_TICK_RECORDS + 1 })
        );
    }

    #[test]
    fn the_wasm_hash_is_fnv_1a() {
        assert_eq!(wasm_hash(b""), crate::hash::FNV_OFFSET);
        assert_eq!(wasm_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
