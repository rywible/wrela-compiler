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
//! distinct finding (by its kind and source line) with the tick it happened in. Exit status: 0
//! no finding, 1 a finding (or the build failed), 2 a usage error.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};
use wrela_abi::input::{
    At, EVENT_SIZE, Event, EventKind, KEYS, PAD_AXES, PAD_BUTTONS, PADS, Scripted, records_at,
};
use wrela_host::{CpuBuild, CpuHost, TickLog};

const USAGE: &str = "usage: wrela fuzz <package-dir> [--ticks <n>] [--time <seconds> | --runs <n>] [--seed <n>] [--fuel <n>] [--seeds <dir>] [--json]";

/// How long a fuzzer runs unless told.
const DEFAULT_TIME: u64 = 60;

struct Args {
    dir: PathBuf,
    ticks: u32,
    limit: Limit,
    seed: u64,
    fuel: u64,
    seeds: Option<PathBuf>,
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
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--ticks" => ticks = it.next()?.parse().ok().filter(|&n| n > 0)?,
            "--runs" => runs = Some(it.next()?.parse().ok().filter(|&n| n > 0)?),
            "--time" => time = Some(it.next()?.parse::<f64>().ok().filter(|&t| t > 0.0)?),
            "--seed" => seed = it.next()?.parse().ok()?,
            "--fuel" => fuel = it.next()?.parse().ok().filter(|&n| n > 0)?,
            "--seeds" => seeds = Some(PathBuf::from(it.next()?)),
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
    Some(Args { dir: dir?, ticks, limit, seed, fuel, seeds, json })
}

/// What a finding is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Trap,
    Hang,
    NotDeterministic,
}

impl Kind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Trap => "trap",
            Kind::Hang => "hang",
            Kind::NotDeterministic => "not-deterministic",
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
    /// What tells findings apart: the kind and the source line (not the column).
    fn key(&self) -> (Kind, Option<String>) {
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
        let mut host = self.start()?;
        let (mut hashes, mut found) = (Vec::new(), None);
        for k in 0..self.ticks {
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
                       "after": f.after})
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

/// Builds the package for fuzzing, into `build/fuzz-build`, and loads it.
fn subject(dir: &Path, ticks: u32, fuel: u64) -> Result<Subject, String> {
    let out = wrela_driver::build_fuzz(dir);
    let built = dir.join("build").join("fuzz-build");
    crate::generated_path(dir, &built)?;
    crate::write_build(&out, &built)?;
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
            if let Some(k) =
                (0..ran.hashes.len()).find(|&k| again.hashes.get(k) != ran.hashes.get(k))
            {
                found = Some(Found {
                    kind: Kind::NotDeterministic,
                    message: format!(
                        "run again, tick {k}'s state hash is {:016x}, not {:016x}",
                        again.hashes.get(k).copied().unwrap_or(0),
                        ran.hashes[k]
                    ),
                    at: None,
                    tick: k as u32,
                });
            } else {
                for (s, &c) in seen.iter_mut().zip(&ran.coverage) {
                    *s |= c != 0;
                }
                corpus.push(input.clone());
            }
        }
        if let Some(f) = found
            && keys.insert(f.key())
        {
            findings.push(Finding { found: f, input, after: executions });
        }
    }
    Ok(Report {
        executions,
        seconds: start.elapsed().as_secs_f64(),
        sites: seen.iter().filter(|&&s| s).count(),
        corpus: corpus.len(),
        findings,
    })
}
