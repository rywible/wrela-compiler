//! `wrela fuzz <package-dir> [--ticks <n>] [--time <seconds> | --runs <n>] [--seed <n>]
//! [--fuel <n>] [--seeds <dir>] [--json]`: finds the input that traps a program's ticks
//! (language.md §16.2). A trap in `@deterministic` code is a trap on every client at the same
//! tick (§12), so one peer's input that causes it stops a whole room.
//!
//! The program is built for fuzzing (`wrela_driver::build_fuzz`: a debug build whose code marks
//! a coverage map) and run in process on the CPU host, on its own thread, so a run is the same
//! each time for a seed. An input is a script of tick records over `--ticks` ticks (64 unless
//! said); each runs on a fresh instance from `init`, each tick given `--fuel` units of work
//! (2^28 unless said). An input that runs code no input ran before joins the corpus, which the
//! next inputs are mutations of: a field changed, an event dropped, copied to another tick or
//! added, two inputs spliced.
//!
//! A finding is a trap (its message and source line), a tick past its fuel (`hang`), or, checked
//! for each input that joins the corpus, a second run of the input whose ticks' state hashes
//! differ from the first's (`not-deterministic`: something the checker let through). The report
//! gives the executions, how many a second, the sites covered, the corpus's size, and each
//! distinct finding (by its kind and source line) with the tick it happened in.
//!
//! Each finding is minimized (Zeller and Hildebrandt's delta debugging): the ticks after it
//! dropped, then halves, quarters, ... of its events down to single ones, then each field
//! toward 0, keeping a change that still gives the same finding. It's written to `--out`
//! (`build/fuzz` unless said) as `<kind>-<hash>.ticks`, a tick log `wrela-host --replay` runs
//! on the build in `build/fuzz-build`, and `<kind>-<hash>.json`, the same as a script.
//!
//! `wrela fuzz <package-dir> --replay <file-or-dir>` runs saved logs and scripts instead, and
//! reports which still fail: a regression check. A log made by another build of the program
//! is run as a script (its records alone), since its hashes are the other build's.
//!
//! `--room` fuzzes the package's server instead, a room of it at a time ([`room`]).
//!
//! Exit status: 0 no finding (or none still fails), 1 a finding (or the build failed), 2 a
//! usage error.

mod room;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};
use wrela_abi::input::{
    At, EVENT_SIZE, Event, EventKind, KEYS, PAD_AXES, PAD_BUTTONS, PADS, Scripted, records_at,
};
use wrela_host::{CpuBuild, CpuHost, TickLog};

const USAGE: &str = "usage: wrela fuzz <package-dir> [--ticks <n>] [--time <seconds> | --runs <n>] [--seed <n>] [--fuel <n>] [--seeds <dir>] [--out <dir>] [--replay <file-or-dir>] [--room] [--json]";

/// How long a fuzzer runs unless told.
const DEFAULT_TIME: u64 = 60;

struct Args {
    dir: PathBuf,
    ticks: u32,
    limit: Limit,
    seed: u64,
    fuel: u64,
    seeds: Option<PathBuf>,
    out: Option<PathBuf>,
    replay: Option<PathBuf>,
    room: bool,
    json: bool,
}

#[derive(Clone, Copy)]
enum Limit {
    Runs(u64),
    Time(Duration),
}

pub fn run(args: &[String]) -> ExitCode {
    let Some(a) = parse(args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if a.room {
        return match fuzz_room(&a) {
            Ok((json, text, failed)) => {
                if a.json {
                    println!("{}", serde_json::to_string_pretty(&json).expect("JSON"));
                } else {
                    print!("{text}");
                }
                if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    if let Some(saved) = &a.replay {
        return match replay(&a, saved) {
            Ok(r) => {
                if a.json {
                    println!("{}", serde_json::to_string_pretty(&r.json()).expect("JSON"));
                } else {
                    print!("{}", r.text());
                }
                if r.0.iter().any(|(_, f)| f.is_some()) {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    match fuzz(&a) {
        Ok(report) => {
            if a.json {
                println!("{}", serde_json::to_string_pretty(&report.json()).expect("JSON"));
            } else {
                print!("{}", report.text());
            }
            if report.findings.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse(args: &[String]) -> Option<Args> {
    let (mut dir, mut ticks, mut runs, mut time) = (None, 64, None, None);
    let (mut seed, mut fuel, mut seeds, mut json) = (0, 1 << 28, None, false);
    let (mut out, mut replay, mut room) = (None, None, false);
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--ticks" => ticks = it.next()?.parse().ok().filter(|&n| n > 0)?,
            "--runs" => runs = Some(it.next()?.parse().ok().filter(|&n| n > 0)?),
            "--time" => time = Some(it.next()?.parse::<f64>().ok().filter(|&t| t > 0.0)?),
            "--seed" => seed = it.next()?.parse().ok()?,
            "--fuel" => fuel = it.next()?.parse().ok().filter(|&n| n > 0)?,
            "--seeds" => seeds = Some(PathBuf::from(it.next()?)),
            "--out" => out = Some(PathBuf::from(it.next()?)),
            "--replay" => replay = Some(PathBuf::from(it.next()?)),
            "--room" => room = true,
            "--json" => json = true,
            _ if dir.is_none() && !x.starts_with('-') => dir = Some(PathBuf::from(x)),
            _ => return None,
        }
    }
    let limit = match (runs, time) {
        (Some(_), Some(_)) => return None,
        (Some(n), None) => Limit::Runs(n),
        (None, Some(t)) => Limit::Time(Duration::from_secs_f64(t)),
        (None, None) => Limit::Time(Duration::from_secs(DEFAULT_TIME)),
    };
    Some(Args { dir: dir?, ticks, limit, seed, fuel, seeds, out, replay, room, json })
}

/// What a finding is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Trap,
    Hang,
    NotDeterministic,
    /// A room's message the program traps on alone (`--room`).
    Poison,
    /// A room that can't go on (`--room`).
    Quarantine,
}

impl Kind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Trap => "trap",
            Kind::Hang => "hang",
            Kind::NotDeterministic => "not-deterministic",
            Kind::Poison => "poison",
            Kind::Quarantine => "quarantine",
        }
    }
}

/// How an input went wrong: what, where in the source (`file:line:column`), and in which tick.
#[derive(Clone, Debug)]
pub(crate) struct Found {
    pub kind: Kind,
    pub message: String,
    pub at: Option<String>,
    pub tick: u32,
}

impl Found {
    /// What tells findings apart: the kind and the source line (not the column); a room's
    /// finding, which has no line, by its message.
    fn key(&self) -> (Kind, Option<String>) {
        if matches!(self.kind, Kind::Poison | Kind::Quarantine) {
            return (self.kind, Some(self.message.clone()));
        }
        let line = self.at.as_ref().map(|a| match a.rsplit_once(':') {
            Some((l, c)) if c.parse::<u32>().is_ok() => l.to_string(),
            _ => a.clone(),
        });
        (self.kind, line)
    }
}

/// A finding and the input that gives it.
struct Finding {
    found: Found,
    input: Vec<Scripted>,
    /// The executions run when it was found.
    after: u64,
    /// Its tick log and its script, once written.
    files: Vec<PathBuf>,
}

/// One run of an input: the sites it ran, each tick's state hash, and what went wrong.
struct Ran {
    coverage: Vec<u8>,
    hashes: Vec<u64>,
    found: Option<Found>,
}

/// The program, built for fuzzing and loaded.
struct Subject {
    build: CpuBuild,
    ticks: u32,
    fuel: u64,
}

impl Subject {
    /// A fresh instance after `init`, its ticker reporting state hashes.
    fn start(&self) -> Result<CpuHost, String> {
        let mut host = self.build.instantiate_in(1, None).map_err(|e| e.to_string())?;
        host.want_hashes(true);
        host.init().map_err(|e| format!("the program's `init` failed: {e}"))?;
        if host.ticker_hz().is_none() {
            return Err("the program starts no ticker (`std::tick::start`) in `init`".into());
        }
        Ok(host)
    }

    /// Runs `input` from `init`, tick by tick, until a tick fails or the last has run.
    fn run(&self, input: &[Scripted]) -> Result<Ran, String> {
        self.run_for(input, self.ticks)
    }

    // Each input starts a fresh instance and runs `init`, rather than putting back a snapshot
    // of memory and globals taken after `init`: on the M4 that start is 22% of a run (0.31 s
    // of 1.40 s over 20,000 runs of a 64-tick input), so a snapshot that cost nothing to put
    // back would run at most 1.28 times as many inputs a second, short of the 2 times that
    // would pay for it (plan step G.5).

    /// [`Subject::run`] over `ticks` ticks.
    fn run_for(&self, input: &[Scripted], ticks: u32) -> Result<Ran, String> {
        let mut host = self.start()?;
        let (mut hashes, mut found) = (Vec::new(), None);
        for k in 0..ticks {
            host.push_records(records_at(input, k));
            match host.tick() {
                Ok(t) => hashes.push(t.hash.unwrap_or(0)),
                Err(e) => {
                    found = Some(self.failed(&host, &e.to_string(), k));
                    break;
                }
            }
        }
        Ok(Ran { coverage: host.coverage().unwrap_or_default(), hashes, found })
    }

    /// What `input` gives of the kind `kind`, if anything: for `not-deterministic`, after a
    /// second run.
    fn reproduce(&self, input: &[Scripted], kind: Kind) -> Result<Option<Found>, String> {
        let ran = self.run(input)?;
        if ran.found.is_some() || kind != Kind::NotDeterministic {
            return Ok(ran.found);
        }
        let again = self.run(input)?;
        Ok(differs(&ran.hashes, &again.hashes))
    }

    fn failed(&self, host: &CpuHost, message: &str, tick: u32) -> Found {
        let failure = host.last_failure();
        let at = failure.and_then(|f| f.at.clone());
        if failure.is_some_and(wrela_host::Failure::out_of_fuel) {
            return Found {
                kind: Kind::Hang,
                message: format!("tick {tick} did more than {} units of work", self.fuel),
                at,
                tick,
            };
        }
        Found { kind: Kind::Trap, message: message.to_string(), at, tick }
    }
}

/// A `not-deterministic` finding, if a second run's hashes aren't the first's.
fn differs(first: &[u64], again: &[u64]) -> Option<Found> {
    let k = (0..first.len().max(again.len())).find(|&k| again.get(k) != first.get(k))?;
    Some(Found {
        kind: Kind::NotDeterministic,
        message: format!(
            "run again, tick {k}'s state hash is {:016x}, not {:016x}",
            again.get(k).copied().unwrap_or(0),
            first.get(k).copied().unwrap_or(0)
        ),
        at: None,
        tick: k as u32,
    })
}

/// splitmix64: a run's random choices, the same for a seed.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x5bd1_e995_9e37_79b9)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number below `n` (`n` > 0).
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    pub(crate) fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// The kinds a new event is drawn from: those with no payload a script can't give. Text and
/// paste events come only from seeds; a post (`std::tick::post`) is the program's own.
const KINDS: [EventKind; 8] = [
    EventKind::PointerMove,
    EventKind::PointerDown,
    EventKind::PointerUp,
    EventKind::Wheel,
    EventKind::KeyDown,
    EventKind::KeyUp,
    EventKind::PadButton,
    EventKind::PadAxis,
];

/// A coordinate: a pixel of a usual canvas, mostly, with an edge now and then.
fn coordinate(rng: &mut Rng) -> f32 {
    match rng.below(16) {
        0 => 0.0,
        1 => 1.0,
        2 => -1.0,
        3 => f32::MAX,
        4 => -f32::MAX,
        5 => rng.unit() * 2048.0,
        _ => rng.below(1024) as f32,
    }
}

/// A number below `n`, its edges (0 and `n - 1`) more often.
fn index(rng: &mut Rng, n: u32) -> u32 {
    match rng.below(8) {
        0 => 0,
        1 => n - 1,
        _ => rng.below(u64::from(n)) as u32,
    }
}

fn modifiers(rng: &mut Rng) -> u32 {
    if rng.chance(4) { rng.below(16) as u32 } else { 0 }
}

/// A new event of `kind`.
fn event(rng: &mut Rng, kind: EventKind) -> Event {
    match kind {
        EventKind::PointerMove | EventKind::PointerDown | EventKind::PointerUp => {
            let (x, y) = (coordinate(rng), coordinate(rng));
            let button = if kind == EventKind::PointerMove { 0 } else { index(rng, 3) };
            Event::pointer(kind, x, y, button, modifiers(rng))
        }
        EventKind::Wheel => {
            let (x, y) = (coordinate(rng), coordinate(rng));
            let d = |rng: &mut Rng| match rng.below(4) {
                0 => 0.0,
                1 => 120.0,
                2 => -120.0,
                _ => (rng.unit() - 0.5) * 2000.0,
            };
            Event::wheel(x, y, d(rng), d(rng), modifiers(rng))
        }
        EventKind::KeyDown | EventKind::KeyUp => {
            // Key 0 is `Unknown`, which a script can't name.
            let key = 1 + index(rng, KEYS.len() as u32 - 1);
            let down = kind == EventKind::KeyDown;
            Event::key(down, key, down && rng.chance(8), modifiers(rng))
        }
        EventKind::PadButton => {
            let pressed = rng.chance(2);
            let value = match rng.below(4) {
                0 => 0.0,
                1 => 1.0,
                _ => rng.unit(),
            };
            Event::pad_button(
                index(rng, PADS),
                index(rng, PAD_BUTTONS.len() as u32),
                value,
                pressed,
            )
        }
        _ => {
            let value = match rng.below(4) {
                0 => 0.0,
                1 => 1.0,
                2 => -1.0,
                _ => rng.unit() * 2.0 - 1.0,
            };
            Event::pad_axis(index(rng, PADS), index(rng, PAD_AXES.len() as u32), value)
        }
    }
}

fn record(tick: u32, event: Event) -> Scripted {
    Scripted { at: At::Tick(tick), event, payload: Vec::new() }
}

fn tick_of(s: &Scripted) -> u32 {
    match s.at {
        At::Tick(t) | At::Frame(t) => t,
    }
}

/// A new input: 0 to 3 events a tick.
fn fresh(rng: &mut Rng, ticks: u32) -> Vec<Scripted> {
    let mut out = Vec::new();
    for t in 0..ticks {
        for _ in 0..rng.below(4) {
            let kind = KINDS[rng.below(KINDS.len() as u64) as usize];
            out.push(record(t, event(rng, kind)));
        }
    }
    out
}

/// `parent` changed one to four times: a field changed, an event dropped, an event copied to
/// another tick, spliced with `other`, or a new event added.
fn mutate(rng: &mut Rng, parent: &[Scripted], other: &[Scripted], ticks: u32) -> Vec<Scripted> {
    let mut out = parent.to_vec();
    for _ in 0..1 + rng.below(4) {
        let n = out.len() as u64;
        match rng.below(5) {
            0 if n > 0 => {
                // One field: the modifiers or a word, from a new event of its kind.
                let i = rng.below(n) as usize;
                let e = &mut out[i].event;
                let kind = if KINDS.contains(&e.kind) { e.kind } else { continue };
                let new = event(rng, kind);
                match rng.below(5) {
                    0 => e.modifiers = new.modifiers,
                    w => e.words[w as usize - 1] = new.words[w as usize - 1],
                }
            }
            1 if n > 0 => {
                out.remove(rng.below(n) as usize);
            }
            2 if n > 0 => {
                let mut e = out[rng.below(n) as usize].clone();
                e.at = At::Tick(rng.below(u64::from(ticks)) as u32);
                out.push(e);
            }
            3 if !other.is_empty() => {
                // This input's ticks before a tick, the other's from it.
                let cut = rng.below(u64::from(ticks)) as u32;
                out.retain(|s| tick_of(s) < cut);
                out.extend(other.iter().filter(|s| tick_of(s) >= cut).cloned());
            }
            _ => {
                let kind = KINDS[rng.below(KINDS.len() as u64) as usize];
                let t = rng.below(u64::from(ticks)) as u32;
                out.push(record(t, event(rng, kind)));
            }
        }
    }
    // A script's ticks don't decrease; events of a tick keep their order.
    out.sort_by_key(tick_of);
    out
}

/// An event from its [`EVENT_SIZE`] bytes, as a tick log keeps it.
pub(crate) fn event_of(bytes: &[u8; EVENT_SIZE as usize]) -> Option<Event> {
    let word = |i: usize| u32::from_le_bytes(bytes[4 * i..4 * i + 4].try_into().expect("4 bytes"));
    let kind = EventKind::ALL.into_iter().find(|k| *k as u32 == word(0))?;
    Some(Event { kind, modifiers: word(1), words: [word(2), word(3), word(4), word(5)] })
}

/// The inputs in `dir`: each `.json` script's tick records, and each `.ticks` log's records.
fn seeds(dir: &Path) -> Result<Vec<Vec<Scripted>>, String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("can't read {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for p in paths {
        match p.extension().and_then(|e| e.to_str()) {
            Some("json") => out.push(read_input(&p)?),
            Some("ticks") => out.push(read_input(&p)?),
            _ => {}
        }
    }
    Ok(out)
}

/// An input from a script (its tick records) or a tick log (its records).
pub(crate) fn read_input(p: &Path) -> Result<Vec<Scripted>, String> {
    let bytes = std::fs::read(p).map_err(|e| format!("can't read {}: {e}", p.display()))?;
    if p.extension().is_some_and(|e| e == "ticks") {
        let log = TickLog::decode(&bytes)
            .map_err(|e| format!("{} isn't a tick log: {e}", p.display()))?;
        return Ok(records_of(&log));
    }
    let text = String::from_utf8(bytes).map_err(|_| format!("{} isn't UTF-8", p.display()))?;
    let script =
        wrela_abi::input::parse_script(&text).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(script.into_iter().filter(|s| matches!(s.at, At::Tick(_))).collect())
}

/// A tick log's records, as an input.
pub(crate) fn records_of(log: &TickLog) -> Vec<Scripted> {
    let mut out = Vec::new();
    for (k, t) in log.ticks.iter().enumerate() {
        out.extend(t.records.iter().filter_map(event_of).map(|e| record(k as u32, e)));
    }
    out
}

struct Report {
    executions: u64,
    seconds: f64,
    sites: usize,
    corpus: usize,
    findings: Vec<Finding>,
}

impl Report {
    fn per_second(&self) -> f64 {
        self.executions as f64 / self.seconds.max(1e-9)
    }

    fn json(&self) -> serde_json::Value {
        use serde_json::json;
        let findings: Vec<serde_json::Value> = self
            .findings
            .iter()
            .map(|f| {
                let (file, line) = place(f.found.at.as_deref());
                json!({"kind": f.found.kind.name(), "message": f.found.message, "file": file,
                       "line": line, "tick": f.found.tick, "events": f.input.len(),
                       "after": f.after, "files": f.files})
            })
            .collect();
        json!({
            "executions": self.executions,
            "seconds": (self.seconds * 1000.0).round() / 1000.0,
            "per_second": self.per_second().round(),
            "sites": self.sites,
            "corpus": self.corpus,
            "findings": findings,
        })
    }

    fn text(&self) -> String {
        let mut s = format!(
            "{} executions in {:.1} s ({:.0} a second), {} sites covered, corpus of {}\n",
            self.executions,
            self.seconds,
            self.per_second(),
            self.sites,
            self.corpus
        );
        if self.findings.is_empty() {
            s += "no findings\n";
        }
        for f in &self.findings {
            let at = f.found.at.as_deref().map(|a| format!(" at {a}")).unwrap_or_default();
            s += &format!(
                "{}{at}, tick {} (found after {} executions): {}\n",
                f.found.kind.name(),
                f.found.tick,
                f.after,
                f.found.message
            );
            for p in &f.files {
                s += &format!("  {}\n", p.display());
            }
        }
        s
    }
}

/// A source location's file and line.
fn place(at: Option<&str>) -> (Option<String>, Option<u32>) {
    let Some(at) = at else { return (None, None) };
    let mut parts = at.rsplitn(3, ':');
    let (_col, line, file) = (parts.next(), parts.next(), parts.next());
    match (file, line.and_then(|l| l.parse().ok())) {
        (Some(f), Some(l)) => (Some(f.to_string()), Some(l)),
        _ => (Some(at.to_string()), None),
    }
}

/// Builds the package for fuzzing, into `build/fuzz-build`: the directory.
fn fuzz_build(dir: &Path) -> Result<PathBuf, String> {
    let out = wrela_driver::build_fuzz(dir);
    let built = dir.join("build").join("fuzz-build");
    crate::generated_path(dir, &built)?;
    crate::write_build(&out, &built)?;
    Ok(built)
}

/// `--room`: the report as JSON and as text, and whether anything was found (or still fails).
fn fuzz_room(a: &Args) -> Result<(serde_json::Value, String, bool), String> {
    use serde_json::json;
    let built = fuzz_build(&a.dir)?;
    let scratch = a.dir.join("build").join("fuzz-room");
    crate::generated_path(&a.dir, &scratch)?;
    let server = room::Server::load(&built, scratch)?;
    if let Some(saved) = &a.replay {
        let r = Replayed(room::replay(&server, saved)?);
        let failed = r.0.iter().any(|(_, f)| f.is_some());
        return Ok((r.json(), r.text(), failed));
    }
    let out = a.out.clone().unwrap_or_else(|| a.dir.join("build").join("fuzz"));
    crate::generated_path(&a.dir, &out)?;
    let start = Instant::now();
    let limit = a.limit;
    let done = move |n: u64| match limit {
        Limit::Runs(k) => n >= k,
        Limit::Time(t) => start.elapsed() >= t,
    };
    let r = room::fuzz(&server, a.seed, a.seeds.as_deref(), done, &out)?;
    let per_second = r.executions as f64 / r.seconds.max(1e-9);
    let findings: Vec<serde_json::Value> = r
        .findings
        .iter()
        .map(|f| {
            json!({"kind": f.found.kind.name(), "message": f.found.message, "event": f.found.tick,
                   "bytes": f.bytes, "events": room::to_json(&f.input), "after": f.after,
                   "files": f.files})
        })
        .collect();
    let mut text = format!(
        "{} executions in {:.1} s ({per_second:.0} a second), {} sites covered, corpus of {}\n",
        r.executions, r.seconds, r.sites, r.corpus
    );
    if r.findings.is_empty() {
        text += "no findings\n";
    }
    for f in &r.findings {
        text += &format!(
            "{} at event {} (found after {} executions): {}\n",
            f.found.kind.name(),
            f.found.tick,
            f.after,
            f.found.message
        );
        if let Some(b) = &f.bytes {
            text += &format!("  the message: {b:?}\n");
        }
        for p in &f.files {
            text += &format!("  {}\n", p.display());
        }
    }
    let json = json!({
        "executions": r.executions,
        "seconds": (r.seconds * 1000.0).round() / 1000.0,
        "per_second": per_second.round(),
        "sites": r.sites,
        "corpus": r.corpus,
        "findings": findings,
    });
    Ok((json, text, !r.findings.is_empty()))
}

/// Builds the package for fuzzing, into `build/fuzz-build`, and loads it.
fn subject(dir: &Path, ticks: u32, fuel: u64) -> Result<Subject, String> {
    let built = fuzz_build(dir)?;
    let build = CpuBuild::load_metered_cpu(&built, fuel).map_err(|e| e.to_string())?.quiet();
    Ok(Subject { build, ticks, fuel })
}

fn fuzz(a: &Args) -> Result<Report, String> {
    let subject = subject(&a.dir, a.ticks, a.fuel)?;
    let mut rng = Rng::new(a.seed);
    let mut seen = vec![false; wrela_abi::COVERAGE_BYTES as usize];
    let mut corpus: Vec<Vec<Scripted>> = Vec::new();
    let mut findings: Vec<Finding> = Vec::new();
    let mut keys = HashSet::new();
    let mut executions = 0u64;
    let start = Instant::now();
    let mut queue = match &a.seeds {
        Some(d) => seeds(d)?,
        None => Vec::new(),
    };
    queue.push(Vec::new());
    queue.reverse();
    let done = |executions: u64| match a.limit {
        Limit::Runs(n) => executions >= n,
        Limit::Time(t) => start.elapsed() >= t,
    };
    while !done(executions) {
        let input = match queue.pop() {
            Some(seed) => seed,
            None if corpus.is_empty() || rng.chance(16) => fresh(&mut rng, a.ticks),
            None => {
                let parent = &corpus[rng.below(corpus.len() as u64) as usize];
                let other = &corpus[rng.below(corpus.len() as u64) as usize];
                mutate(&mut rng, parent, other, a.ticks)
            }
        };
        let ran = subject.run(&input)?;
        executions += 1;
        let mut found = ran.found;
        let new = ran.coverage.iter().zip(&seen).any(|(&c, &s)| c != 0 && !s);
        if found.is_none() && new {
            // Once for each input that joins the corpus: the same hashes a second time.
            let again = subject.run(&input)?;
            executions += 1;
            found = differs(&ran.hashes, &again.hashes);
            if found.is_none() {
                for (s, &c) in seen.iter_mut().zip(&ran.coverage) {
                    *s |= c != 0;
                }
                corpus.push(input.clone());
            }
        }
        if let Some(f) = found
            && keys.insert(f.key())
        {
            findings.push(Finding { found: f, input, after: executions, files: Vec::new() });
        }
    }
    let seconds = start.elapsed().as_secs_f64();
    let out = a.out.clone().unwrap_or_else(|| a.dir.join("build").join("fuzz"));
    if !findings.is_empty() {
        crate::generated_path(&a.dir, &out)?;
        std::fs::create_dir_all(&out).map_err(|e| format!("can't make {}: {e}", out.display()))?;
    }
    for f in &mut findings {
        (f.input, f.found) = minimize(&subject, std::mem::take(&mut f.input), f.found.clone())?;
        f.files = write_finding(&subject, &out, f)?;
    }
    Ok(Report {
        executions,
        seconds,
        sites: seen.iter().filter(|&&s| s).count(),
        corpus: corpus.len(),
        findings,
    })
}

/// Whether a field of an event is a float, and the least value it takes (a key's is 1: key 0 is
/// `Unknown`, which a script can't name). `None`: not a field of the event's kind, or an event
/// a seed gave, which is kept as it is.
fn field(kind: EventKind, slot: usize) -> Option<(bool, u32)> {
    use EventKind as K;
    // Slot 0 is the modifiers, 1 to 4 the words.
    match (kind, slot) {
        (_, 0) if KINDS.contains(&kind) => Some((false, 0)),
        (K::PointerMove | K::PointerDown | K::PointerUp, 1 | 2) => Some((true, 0)),
        (K::PointerDown | K::PointerUp, 3) => Some((false, 0)),
        (K::Wheel, 1..=4) => Some((true, 0)),
        (K::KeyDown | K::KeyUp, 1) => Some((false, 1)),
        (K::KeyDown, 2) => Some((false, 0)),
        (K::PadButton, 1 | 2 | 4) | (K::PadAxis, 1 | 2) => Some((false, 0)),
        (K::PadButton | K::PadAxis, 3) => Some((true, 0)),
        _ => None,
    }
}

fn slot(e: &mut Event, slot: usize) -> &mut u32 {
    if slot == 0 { &mut e.modifiers } else { &mut e.words[slot - 1] }
}

/// `input` made as small as it can be while it still gives `found` (the same kind at the same
/// line): the ticks after it dropped, then chunks of its events (halves, quarters, ... single
/// events), then each field toward its least value.
fn minimize(
    subject: &Subject,
    input: Vec<Scripted>,
    found: Found,
) -> Result<(Vec<Scripted>, Found), String> {
    let key = found.key();
    let same = |cand: &[Scripted]| -> Result<Option<Found>, String> {
        Ok(subject.reproduce(cand, found.kind)?.filter(|f| f.key() == key))
    };
    let (mut best, mut now) = (input, found.clone());
    let before: Vec<Scripted> = best.iter().filter(|s| tick_of(s) <= now.tick).cloned().collect();
    if before.len() < best.len()
        && let Some(f) = same(&before)?
    {
        (best, now) = (before, f);
    }
    let mut size = (best.len() / 2).max(1);
    while !best.is_empty() {
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
        for k in 0..5 {
            let Some((float, least)) = field(best[i].event.kind, k) else { continue };
            // The least value; else halves toward it, while they still give the finding.
            let mut tries = vec![least];
            let mut v = *slot(&mut best[i].event, k);
            for _ in 0..24 {
                v = if float {
                    let x = f32::from_bits(v);
                    if x.abs() < 1.0 {
                        break;
                    }
                    (x / 2.0).trunc().to_bits()
                } else {
                    if v / 2 < least {
                        break;
                    }
                    v / 2
                };
                tries.push(v);
            }
            for t in tries {
                if *slot(&mut best[i].event, k) == t {
                    break;
                }
                let mut cand = best.clone();
                *slot(&mut cand[i].event, k) = t;
                match same(&cand)? {
                    Some(f) => {
                        (best, now) = (cand, f);
                        break;
                    }
                    None if t == least => {}
                    None => break,
                }
            }
        }
    }
    Ok((best, now))
}

/// FNV-1a 64 of `text`: a finding's name, the same for the same kind and line.
fn fnv(text: &str) -> u64 {
    text.bytes()
        .fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

/// Writes a finding's tick log and script into `out`: their paths.
fn write_finding(subject: &Subject, out: &Path, f: &Finding) -> Result<Vec<PathBuf>, String> {
    let (kind, line) = f.found.key();
    let name = format!("{}-{:016x}", kind.name(), fnv(&format!("{}:{line:?}", kind.name())));
    let log = log_of(subject, &f.input, &f.found)?;
    let ticks = out.join(format!("{name}.ticks"));
    let script = out.join(format!("{name}.json"));
    let write = |p: &Path, bytes: &[u8]| {
        std::fs::write(p, bytes).map_err(|e| format!("can't write {}: {e}", p.display()))
    };
    write(&ticks, &log.encode())?;
    let json = serde_json::to_string_pretty(&script_json(&f.input)).expect("JSON") + "\n";
    write(&script, json.as_bytes())?;
    Ok(vec![ticks, script])
}

/// The tick log of `input` up to the tick of `found`: each tick before it with its state hash,
/// then that tick with its records (a hash of 0: it has none, it fails). A `not-deterministic`
/// finding's log is every tick of one run.
fn log_of(subject: &Subject, input: &[Scripted], found: &Found) -> Result<TickLog, String> {
    let wrap = |e: wrela_host::Error| e.to_string();
    if found.kind == Kind::NotDeterministic {
        return subject.build.record_ticks(subject.ticks, input, 1).map_err(wrap);
    }
    let mut log = subject.build.record_ticks(found.tick, input, 1).map_err(wrap)?;
    let records = records_at(input, found.tick).map(|e| e.bytes()).collect();
    log.ticks.push(wrela_abi::ticks::Tick { records, hash: 0 });
    Ok(log)
}

/// An input as a script ([`wrela_abi::input`]'s format), each event with its tick. A seed's
/// event a script can't give alone (a post, a character of a composition) is left out.
fn script_json(input: &[Scripted]) -> serde_json::Value {
    use EventKind as K;
    use serde_json::json;
    let mut out = Vec::new();
    for s in input {
        let e = &s.event;
        let f = |w: u32| f64::from(f32::from_bits(e.words[w as usize]));
        let w = |i: usize| e.words[i];
        let mut o = match e.kind {
            K::PointerMove => json!({"type": "move", "x": f(0), "y": f(1)}),
            K::PointerDown => json!({"type": "down", "x": f(0), "y": f(1), "button": w(2)}),
            K::PointerUp => json!({"type": "up", "x": f(0), "y": f(1), "button": w(2)}),
            K::Wheel => json!({"type": "wheel", "x": f(0), "y": f(1), "dx": f(2), "dy": f(3)}),
            K::KeyDown => json!({"type": "keydown", "code": w(0), "repeat": w(1) != 0}),
            K::KeyUp => json!({"type": "keyup", "code": w(0)}),
            K::PadButton => json!({"type": "padbutton", "pad": w(0), "button": w(1),
                                   "value": f(2), "pressed": w(3) != 0}),
            K::PadAxis => json!({"type": "padaxis", "pad": w(0), "axis": w(1), "value": f(2)}),
            K::Text => match char::from_u32(w(0)) {
                Some(c) => json!({"type": "text", "text": c.to_string()}),
                None => continue,
            },
            K::Paste => json!({"type": "paste", "serial": w(0)}),
            K::Assist => json!({"type": "assist", "node": w(0), "action": w(1)}),
            K::Field => json!({"type": "field", "node": w(0), "revision": w(1)}),
            _ => continue,
        };
        o["tick"] = json!(tick_of(s));
        for (name, bit) in [("shift", 1), ("ctrl", 2), ("alt", 4), ("meta", 8)] {
            if KINDS.contains(&e.kind) && e.modifiers & bit != 0 {
                o[name] = json!(true);
            }
        }
        out.push(o);
    }
    json!(out)
}

/// `--replay`: each saved file, and what it gives now, if anything.
struct Replayed(Vec<(PathBuf, Option<Found>)>);

impl Replayed {
    fn json(&self) -> serde_json::Value {
        use serde_json::json;
        let files: Vec<serde_json::Value> = self
            .0
            .iter()
            .map(|(p, f)| match f {
                None => json!({"path": p, "fails": false}),
                Some(f) => {
                    let (file, line) = place(f.at.as_deref());
                    json!({"path": p, "fails": true, "kind": f.kind.name(),
                           "message": f.message, "file": file, "line": line, "tick": f.tick})
                }
            })
            .collect();
        json!({ "replayed": files })
    }

    fn text(&self) -> String {
        let mut s = String::new();
        for (p, f) in &self.0 {
            match f {
                None => s += &format!("{}: passes\n", p.display()),
                Some(f) => {
                    let at = f.at.as_deref().map(|a| format!(" at {a}")).unwrap_or_default();
                    s += &format!(
                        "{}: {}{at}, tick {}: {}\n",
                        p.display(),
                        f.kind.name(),
                        f.tick,
                        f.message
                    );
                }
            }
        }
        let failing = self.0.iter().filter(|(_, f)| f.is_some()).count();
        s += &format!("{failing} of {} still fail\n", self.0.len());
        s
    }
}

/// Runs each saved log and script in `saved` (a file, or a directory of them).
fn replay(a: &Args, saved: &Path) -> Result<Replayed, String> {
    let subject = subject(&a.dir, a.ticks, a.fuel)?;
    let paths = if saved.is_dir() {
        let mut ps: Vec<PathBuf> = std::fs::read_dir(saved)
            .map_err(|e| format!("can't read {}: {e}", saved.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "ticks" || e == "json"))
            .collect();
        ps.sort();
        ps
    } else {
        vec![saved.to_path_buf()]
    };
    let mut out = Vec::new();
    for p in paths {
        let found = replay_one(&subject, &p)?;
        out.push((p, found));
    }
    Ok(Replayed(out))
}

fn replay_one(subject: &Subject, p: &Path) -> Result<Option<Found>, String> {
    let input = read_input(p)?;
    let ticks = input.iter().map(|s| tick_of(s) + 1).max().unwrap_or(0).max(subject.ticks);
    if p.extension().is_some_and(|e| e == "ticks") {
        let bytes = std::fs::read(p).map_err(|e| format!("can't read {}: {e}", p.display()))?;
        let log = TickLog::decode(&bytes).map_err(|e| format!("{}: {e}", p.display()))?;
        if log.wasm_hash == subject.build.wasm_hash() {
            // This build's log: its ticks, each hash checked.
            use wrela_host::ReplayError as R;
            let mut host = subject.start()?;
            let n = log.ticks.len() as u32;
            return Ok(match host.replay_ticks(&log, n) {
                Ok(()) => None,
                Err(R::Failed { tick: Some(k), error }) => {
                    Some(subject.failed(&host, &error.to_string(), k))
                }
                Err(R::Tick { tick, log, replayed }) => Some(Found {
                    kind: Kind::NotDeterministic,
                    message: format!("tick {tick}'s state hash is {replayed:016x}, not {log:016x}"),
                    at: None,
                    tick,
                }),
                Err(e) => return Err(format!("{}: {e}", p.display())),
            });
        }
    }
    Ok(subject.run_for(&input, ticks)?.found)
}
