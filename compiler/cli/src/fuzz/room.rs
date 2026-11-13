//! `wrela fuzz <package-dir> --room`: the package's server build, a room of it driven through
//! the room host (`wrela_host::rooms::RoomHost`) in a scratch directory, as `wrela run` drives
//! one. An input is a list of events: a connection opens, a message of bytes arrives from one,
//! a connection closes, time passes. New messages are 0 to 64 random bytes; mutating one flips
//! bits, changes its length, repeats it, or takes bytes from a seed's. Coverage is the server
//! module's map, read after each wake.
//!
//! A finding is `poison`, a message the program traps on alone (the room goes on, and refuses
//! it poisoned, but its reducer has a bug: the bytes are the report), or `quarantine`, a room
//! that can't go on. Each is minimized (events dropped, the message's bytes cut and zeroed) and
//! written as `<kind>-<hash>.json`, the events, and `<kind>-<hash>.bin`, the message.

use super::{Found, Kind, Rng, fnv};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use wrela_host::rooms::{RoomHost, Status};
use wrela_host::server::{Limits, ServerBuild};

/// The connections an input uses: not the journal's (0), not a poisoned one's.
const CONNS: u32 = 4;
/// The longest message a new one is.
const MOST_BYTES: u64 = 64;

/// One event of a room's input.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Ev {
    Open(u32),
    Message(u32, Vec<u8>),
    Close(u32),
    /// Time passes: this many seconds.
    Wait(f32),
}

/// The server build, and where its room runs.
pub(crate) struct Server {
    build: Arc<ServerBuild>,
    hash: u64,
    scratch: PathBuf,
}

/// A finding of a run, and the message it was about, if one.
type Got = (Found, Option<Vec<u8>>);

/// What one run of an input gave.
struct Ran {
    coverage: Vec<u8>,
    found: Option<Got>,
}

impl Server {
    /// The server build in `built` (a fuzzer's build), its room in `scratch`.
    pub(crate) fn load(built: &Path, scratch: PathBuf) -> Result<Server, String> {
        let wasm_path = built.join("server").join("game.wasm");
        let wasm = std::fs::read(&wasm_path).map_err(|_| {
            "the package has no server build: `--room` fuzzes a program with `serve`".to_string()
        })?;
        let build = ServerBuild::from_wasm(&wasm).map_err(|e| format!("{e}"))?;
        std::fs::create_dir_all(&scratch)
            .map_err(|e| format!("can't make {}: {e}", scratch.display()))?;
        Ok(Server { build: Arc::new(build), hash: wrela_abi::ticks::wasm_hash(&wasm), scratch })
    }

    /// Runs `input` on a new room (its journal removed first), waking it after each event.
    fn run(&self, input: &[Ev]) -> Result<Ran, String> {
        let _ = std::fs::remove_file(self.scratch.join("fuzz.journal"));
        let mut room = RoomHost::open(
            self.build.clone(),
            self.hash,
            &self.scratch,
            "fuzz",
            Limits::default(),
            None,
        )
        .map_err(|e| format!("the room: {e}"))?;
        let mut coverage = vec![0u8; wrela_abi::COVERAGE_BYTES as usize];
        let mut seen = |room: &RoomHost| {
            if let Some(map) = room.coverage() {
                for (c, m) in coverage.iter_mut().zip(map) {
                    *c |= m;
                }
            }
        };
        let mut time = 0.0f32;
        let mut found = None;
        for (i, e) in input.iter().enumerate() {
            match e {
                Ev::Open(c) => room.connect(*c),
                Ev::Close(c) => room.disconnect(*c),
                Ev::Message(c, bytes) => room.receive(*c, bytes.clone()),
                Ev::Wait(s) => time += s,
            }
            let woke = room.wake(time);
            seen(&room);
            let _ = room.take_outbox();
            if let Some((_, bytes, why)) = room.take_poisoned().into_iter().next() {
                let f = Found { kind: Kind::Poison, message: why, at: None, tick: i as u32 };
                found = Some((f, Some(bytes)));
                break;
            }
            if let Status::Quarantined(why) = room.status() {
                let bytes = match e {
                    Ev::Message(_, b) => Some(b.clone()),
                    _ => None,
                };
                let f = Found {
                    kind: Kind::Quarantine,
                    message: why.clone(),
                    at: None,
                    tick: i as u32,
                };
                found = Some((f, bytes));
                break;
            }
            // A wake past the host's limits isn't the program's bug: the room starts again.
            let _ = woke;
        }
        Ok(Ran { coverage, found })
    }
}

fn message(rng: &mut Rng) -> Vec<u8> {
    let n = rng.below(MOST_BYTES + 1) as usize;
    (0..n).map(|_| rng.next() as u8).collect()
}

fn conn(rng: &mut Rng) -> u32 {
    1 + rng.below(u64::from(CONNS)) as u32
}

fn event(rng: &mut Rng) -> Ev {
    match rng.below(8) {
        0 => Ev::Open(conn(rng)),
        1 => Ev::Close(conn(rng)),
        2 => Ev::Wait(match rng.below(3) {
            0 => 0.016,
            1 => 1.0,
            _ => 30.0,
        }),
        _ => Ev::Message(conn(rng), message(rng)),
    }
}

/// A new input: each connection opens, then 1 to 16 events.
fn fresh(rng: &mut Rng) -> Vec<Ev> {
    let mut out: Vec<Ev> = (1..=CONNS).map(Ev::Open).collect();
    for _ in 0..1 + rng.below(16) {
        out.push(event(rng));
    }
    out
}

/// A message's bytes changed: bits flipped, its length changed, or bytes from `donor`.
fn bytes_of(rng: &mut Rng, b: &mut Vec<u8>, donor: Option<&[u8]>) {
    match rng.below(4) {
        0 if !b.is_empty() => {
            let i = rng.below(b.len() as u64) as usize;
            b[i] ^= 1 << rng.below(8);
        }
        1 => {
            let n = rng.below(MOST_BYTES + 1) as usize;
            b.resize(n, rng.next() as u8);
        }
        2 if donor.is_some_and(|d| !d.is_empty()) => {
            let d = donor.expect("a donor");
            let from = rng.below(d.len() as u64) as usize;
            let to = from + 1 + rng.below((d.len() - from) as u64) as usize;
            let at = rng.below(b.len() as u64 + 1) as usize;
            b.splice(at..at, d[from..to].iter().copied());
        }
        _ if !b.is_empty() => {
            let i = rng.below(b.len() as u64) as usize;
            b[i] = rng.next() as u8;
        }
        _ => b.push(rng.next() as u8),
    }
}

/// `parent` changed one to four times: a message's bytes, an event dropped, repeated or added.
fn mutate(rng: &mut Rng, parent: &[Ev], other: &[Ev]) -> Vec<Ev> {
    let mut out = parent.to_vec();
    let messages = |v: &[Ev]| -> Vec<Vec<u8>> {
        v.iter()
            .filter_map(|e| if let Ev::Message(_, b) = e { Some(b.clone()) } else { None })
            .collect()
    };
    let donors = messages(other);
    for _ in 0..1 + rng.below(4) {
        let n = out.len() as u64;
        let ms: Vec<usize> =
            (0..out.len()).filter(|&i| matches!(out[i], Ev::Message(..))).collect();
        match rng.below(5) {
            0 | 1 if !ms.is_empty() => {
                let i = ms[rng.below(ms.len() as u64) as usize];
                let donor = (!donors.is_empty())
                    .then(|| donors[rng.below(donors.len() as u64) as usize].clone());
                if let Ev::Message(_, b) = &mut out[i] {
                    bytes_of(rng, b, donor.as_deref());
                }
            }
            2 if n > 0 => {
                out.remove(rng.below(n) as usize);
            }
            3 if !ms.is_empty() => {
                let e = out[ms[rng.below(ms.len() as u64) as usize]].clone();
                let at = rng.below(n + 1) as usize;
                out.insert(at, e);
            }
            _ => {
                let at = rng.below(n + 1) as usize;
                out.insert(at, event(rng));
            }
        }
    }
    out
}

/// A room's input as JSON: a list of `{"open": c}`, `{"close": c}`, `{"message": c, "bytes":
/// [...]}` and `{"wait": seconds}`.
pub(crate) fn to_json(input: &[Ev]) -> serde_json::Value {
    use serde_json::json;
    json!(
        input
            .iter()
            .map(|e| match e {
                Ev::Open(c) => json!({"open": c}),
                Ev::Close(c) => json!({"close": c}),
                Ev::Message(c, b) => json!({"message": c, "bytes": b}),
                Ev::Wait(s) => json!({"wait": s}),
            })
            .collect::<Vec<_>>()
    )
}

/// A room's input from its JSON ([`to_json`]).
pub(crate) fn from_json(text: &str) -> Result<Vec<Ev>, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let items = v.as_array().ok_or("a room's input is a JSON array of events")?;
    let conn = |x: &serde_json::Value| -> Result<u32, String> {
        x.as_u64()
            .filter(|&c| c >= 1 && c <= u64::from(CONNS))
            .map(|c| c as u32)
            .ok_or_else(|| format!("a connection is 1 to {CONNS}"))
    };
    items
        .iter()
        .map(|o| {
            if let Some(c) = o.get("open") {
                Ok(Ev::Open(conn(c)?))
            } else if let Some(c) = o.get("close") {
                Ok(Ev::Close(conn(c)?))
            } else if let Some(c) = o.get("message") {
                let bytes = o["bytes"]
                    .as_array()
                    .and_then(|a| {
                        a.iter().map(|x| x.as_u64().filter(|&b| b < 256).map(|b| b as u8)).collect()
                    })
                    .ok_or("`bytes` is an array of numbers from 0 to 255")?;
                Ok(Ev::Message(conn(c)?, bytes))
            } else if let Some(s) = o.get("wait").and_then(serde_json::Value::as_f64) {
                Ok(Ev::Wait(s as f32))
            } else {
                Err(format!("{o} isn't a room's event"))
            }
        })
        .collect()
}

/// The inputs of a seed directory: each `.json` room input, and each `.bin` file as a message
/// from a connection that opened.
fn seeds(dir: &Path) -> Result<Vec<Vec<Ev>>, String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("can't read {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for p in paths {
        let read = |p: &Path| std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()));
        match p.extension().and_then(|e| e.to_str()) {
            Some("json") => out.push(
                from_json(&String::from_utf8_lossy(&read(&p)?))
                    .map_err(|e| format!("{}: {e}", p.display()))?,
            ),
            Some("bin") => out.push(vec![Ev::Open(1), Ev::Message(1, read(&p)?)]),
            _ => {}
        }
    }
    Ok(out)
}

/// A finding of a room, and its input.
pub(crate) struct Finding {
    pub found: Found,
    pub input: Vec<Ev>,
    pub bytes: Option<Vec<u8>>,
    pub after: u64,
    pub files: Vec<PathBuf>,
}

pub(crate) struct Report {
    pub executions: u64,
    pub seconds: f64,
    pub sites: usize,
    pub corpus: usize,
    pub findings: Vec<Finding>,
}

/// The fuzzing loop for a room: as for ticks, with a room's inputs.
pub(crate) fn fuzz(
    server: &Server,
    seed: u64,
    seed_dir: Option<&Path>,
    done: impl Fn(u64) -> bool,
    out: &Path,
) -> Result<Report, String> {
    let mut rng = Rng::new(seed);
    let mut seen = vec![false; wrela_abi::COVERAGE_BYTES as usize];
    let (mut corpus, mut findings): (Vec<Vec<Ev>>, Vec<Finding>) = (Vec::new(), Vec::new());
    let mut keys = HashSet::new();
    let mut executions = 0u64;
    let start = Instant::now();
    let mut queue = match seed_dir {
        Some(d) => seeds(d)?,
        None => Vec::new(),
    };
    queue.reverse();
    while !done(executions) {
        let input = match queue.pop() {
            Some(s) => s,
            None if corpus.is_empty() || rng.chance(16) => fresh(&mut rng),
            None => {
                let parent = &corpus[rng.below(corpus.len() as u64) as usize];
                let other = &corpus[rng.below(corpus.len() as u64) as usize];
                mutate(&mut rng, parent, other)
            }
        };
        let ran = server.run(&input)?;
        executions += 1;
        match ran.found {
            Some((f, bytes)) => {
                if keys.insert(f.key()) {
                    findings.push(Finding {
                        found: f,
                        input,
                        bytes,
                        after: executions,
                        files: vec![],
                    });
                }
            }
            None => {
                if ran.coverage.iter().zip(&seen).any(|(&c, &s)| c != 0 && !s) {
                    for (s, &c) in seen.iter_mut().zip(&ran.coverage) {
                        *s |= c != 0;
                    }
                    corpus.push(input);
                }
            }
        }
    }
    let seconds = start.elapsed().as_secs_f64();
    if !findings.is_empty() {
        std::fs::create_dir_all(out).map_err(|e| format!("can't make {}: {e}", out.display()))?;
    }
    for f in &mut findings {
        let (input, (found, bytes)) = minimize(server, std::mem::take(&mut f.input), &f.found)?;
        (f.input, f.found, f.bytes) = (input, found, bytes);
        f.files = write(out, f)?;
    }
    Ok(Report {
        executions,
        seconds,
        sites: seen.iter().filter(|&&s| s).count(),
        corpus: corpus.len(),
        findings,
    })
}

/// `input` made as small as it can be while it still gives the same finding: events dropped,
/// in chunks then one by one, then each message's bytes cut from the end and zeroed.
fn minimize(server: &Server, input: Vec<Ev>, found: &Found) -> Result<(Vec<Ev>, Got), String> {
    let key = found.key();
    let same = |cand: &[Ev]| -> Result<Option<Got>, String> {
        Ok(server.run(cand)?.found.filter(|(f, _)| f.key() == key))
    };
    let Some(mut now) = same(&input)? else {
        return Ok((input, (found.clone(), None)));
    };
    let mut best = input;
    let mut size = (best.len() / 2).max(1);
    loop {
        let mut i = 0;
        while i < best.len() {
            let mut cand = best[..i].to_vec();
            cand.extend_from_slice(&best[(i + size).min(best.len())..]);
            match same(&cand)? {
                Some(f) => (best, now) = (cand, f),
                None => i += size,
            }
        }
        if size == 1 {
            break;
        }
        size = (size / 2).max(1);
    }
    for i in 0..best.len() {
        let Ev::Message(c, bytes) = best[i].clone() else { continue };
        let mut b = bytes;
        while !b.is_empty() {
            let mut cand = best.clone();
            cand[i] = Ev::Message(c, b[..b.len() - 1].to_vec());
            match same(&cand)? {
                Some(f) => {
                    (best, now) = (cand, f);
                    b.pop();
                }
                None => break,
            }
        }
        for k in 0..b.len() {
            if b[k] == 0 {
                continue;
            }
            let mut zeroed = b.clone();
            zeroed[k] = 0;
            let mut cand = best.clone();
            cand[i] = Ev::Message(c, zeroed.clone());
            if let Some(f) = same(&cand)? {
                (best, now) = (cand, f);
                b = zeroed;
            }
        }
    }
    Ok((best, now))
}

fn write(out: &Path, f: &Finding) -> Result<Vec<PathBuf>, String> {
    let name = format!(
        "{}-{:016x}",
        f.found.kind.name(),
        fnv(&format!("{}:{}", f.found.kind.name(), f.found.message))
    );
    let w = |p: &Path, bytes: &[u8]| {
        std::fs::write(p, bytes).map_err(|e| format!("can't write {}: {e}", p.display()))
    };
    let events = out.join(format!("{name}.json"));
    w(
        &events,
        (serde_json::to_string_pretty(&to_json(&f.input)).expect("JSON") + "\n").as_bytes(),
    )?;
    let mut files = vec![events];
    if let Some(b) = &f.bytes {
        let bin = out.join(format!("{name}.bin"));
        w(&bin, b)?;
        files.push(bin);
    }
    Ok(files)
}

/// `--room --replay`: each saved room input (`.json`) in `saved`, and what it gives now.
pub(crate) fn replay(
    server: &Server,
    saved: &Path,
) -> Result<Vec<(PathBuf, Option<Found>)>, String> {
    let paths = if saved.is_dir() {
        let mut ps: Vec<PathBuf> = std::fs::read_dir(saved)
            .map_err(|e| format!("can't read {}: {e}", saved.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        ps.sort();
        ps
    } else {
        vec![saved.to_path_buf()]
    };
    let mut out = Vec::new();
    for p in paths {
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let input = from_json(&text).map_err(|e| format!("{}: {e}", p.display()))?;
        out.push((p, server.run(&input)?.found.map(|(f, _)| f)));
    }
    Ok(out)
}
