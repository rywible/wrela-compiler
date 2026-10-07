//! Hot reload (#28 §9, language.md §22): `wrela run <package>` builds the program lifted, serves
//! it to Chrome on 127.0.0.1, and watches its files and its dependencies'. A change that touches
//! only lifted literals goes to the running program as their new values (no build); any other
//! change builds the program again, and the page swaps the new build in where the old one was,
//! without reloading (runtime/browser's live.ts). The native host's [`Watcher`] user does the
//! same with `wrela_host::Host::set_literal` and `Host::reload`.
//!
//! ```text
//!   files ──scan, 20 ms──▶ Watcher ──literals only──▶ Change::Literals ──▶ __lift_set
//!                            │
//!                            └──anything else──build──▶ Change::Built ──▶ the program swapped
//! ```
//!
//! The server's paths: `/` and the build's files (the newest build), `/v<n>/...` (build `n`),
//! `/live/next?after=<seq>` (a long poll: the changes after `seq`, as JSON), `/live/shown` (the
//! page says which frame first showed a change), `/results/<file>` (test mode's results).

use crate::http;
use crate::studio::{Numbers, moved, same_but_numbers};
use std::collections::BTreeMap;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// How often the watcher looks at the files.
pub const SCAN: Duration = Duration::from_millis(20);

/// A file as the watcher saw it: when it was last changed, its length, and its text.
#[derive(Clone, Debug, PartialEq)]
struct Seen {
    modified: SystemTime,
    len: u64,
    text: String,
}

type Files = BTreeMap<PathBuf, Seen>;

/// What changed in the program's files.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// Lifted literals alone: each one's index in the build, and its new value.
    Literals(Vec<(u32, f32)>),
    /// Anything else: the program built again, into `dir`, its `version`th build.
    Built { version: u64, dir: PathBuf },
    /// Anything else, and the program doesn't build: why, rendered.
    Failed(String),
}

/// The lifted build's report, as far as the watcher reads it (`lift.json`).
#[derive(serde::Deserialize)]
struct Report {
    files: Vec<ReportFile>,
    literals: Vec<ReportLiteral>,
}

#[derive(serde::Deserialize)]
struct ReportFile {
    /// Relative to the program's package.
    path: String,
    text: String,
}

#[derive(serde::Deserialize)]
struct ReportLiteral {
    file: u32,
    start: u32,
    end: u32,
    value: f32,
}

/// A program, built lifted, and its files watched (see the module's docs).
pub struct Watcher {
    pkg: PathBuf,
    lift: Vec<String>,
    debug: bool,
    /// Where the builds go: `out/<version>`.
    out: PathBuf,
    version: u64,
    /// The packages' directories.
    dirs: Vec<PathBuf>,
    /// The files when last looked at, and as the newest build saw them.
    seen: Files,
    built: Files,
    report: Report,
    /// Each literal's value now: the build's, then each change's.
    values: Vec<f32>,
    /// When the newest change was saved: the newest time a changed file was modified.
    pub saved: Option<SystemTime>,
}

impl Watcher {
    /// Builds the program at `pkg` lifted (the packages `lift` names, by default the program's
    /// own) into `out/1`, and starts watching its files.
    pub fn start(pkg: &Path, out: &Path, lift: &[String], debug: bool) -> Result<Watcher, String> {
        let packages = crate::packages(pkg)?;
        let lift = if lift.is_empty() { vec![packages[0].0.clone()] } else { lift.to_vec() };
        let dirs = packages.into_iter().map(|(_, d)| d).collect();
        let mut w = Watcher {
            pkg: pkg.to_path_buf(),
            lift,
            debug,
            out: out.to_path_buf(),
            version: 0,
            dirs,
            seen: Files::new(),
            built: Files::new(),
            report: Report { files: Vec::new(), literals: Vec::new() },
            values: Vec::new(),
            saved: None,
        };
        w.seen = w.scan(&Files::new());
        match w.build() {
            Change::Failed(why) => Err(why),
            _ => Ok(w),
        }
    }

    /// The newest build's directory.
    pub fn dir(&self) -> PathBuf {
        self.out.join(self.version.to_string())
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// The program's files and its dependencies' (modules and manifests, not build output),
    /// read again only where a file's time or length has changed since `before`.
    fn scan(&self, before: &Files) -> Files {
        let mut out = Files::new();
        fn walk(dir: &Path, top: bool, before: &Files, out: &mut Files) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let path = e.path();
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let Ok(meta) = e.metadata() else { continue };
                if meta.is_dir() {
                    if !(top && ["build", "results", "target"].contains(&name.as_str())) {
                        walk(&path, false, before, out);
                    }
                } else if name.ends_with(".wrela") || name == "wrela.toml" {
                    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    let len = meta.len();
                    let seen = match before.get(&path) {
                        Some(s) if s.modified == modified && s.len == len => s.clone(),
                        _ => Seen { modified, len, text: std::fs::read_to_string(&path).unwrap_or_default() },
                    };
                    out.insert(path, seen);
                }
            }
        }
        for d in &self.dirs {
            walk(d, true, before, &mut out);
        }
        out
    }

    /// Looks at the files once: what's changed since the last look, if anything.
    pub fn poll(&mut self) -> Option<Change> {
        let now = self.scan(&self.seen);
        let changed: Vec<&PathBuf> = now
            .iter()
            .filter(|(p, s)| self.seen.get(*p).is_none_or(|old| old.text != s.text))
            .map(|(p, _)| p)
            .chain(self.seen.keys().filter(|p| !now.contains_key(*p)))
            .collect();
        if changed.is_empty() {
            self.seen = now;
            return None;
        }
        self.saved = changed.iter().filter_map(|p| now.get(*p)).map(|s| s.modified).max();
        let change = match self.literal_values(&now) {
            Some(values) => {
                let edits: Vec<(u32, f32)> = values
                    .iter()
                    .enumerate()
                    .filter(|(i, v)| self.values[*i].to_bits() != v.to_bits())
                    .map(|(i, v)| (i as u32, *v))
                    .collect();
                self.values = values;
                // A change of a literal back and forth since the last look is no change.
                (!edits.is_empty()).then_some(Change::Literals(edits))
            }
            None => {
                self.seen = now.clone();
                Some(self.build())
            }
        };
        self.seen = now;
        change
    }

    /// Builds the program as its files are now, into the next version's directory.
    fn build(&mut self) -> Change {
        let out = match crate::build_lifted(&self.pkg, &self.lift, self.debug) {
            Ok(out) => out,
            Err(why) => return Change::Failed(why),
        };
        if out.has_errors() {
            return Change::Failed(wrela_diag::render::render_all(&out.sources, &out.diagnostics));
        }
        let version = self.version + 1;
        let dir = self.out.join(version.to_string());
        if let Err(e) = out.write_to(&dir) {
            return Change::Failed(format!("can't write the build to {}: {e}", dir.display()));
        }
        let report = std::fs::read_to_string(dir.join("lift.json"))
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Report>(&t).map_err(|e| e.to_string()));
        let report = match report {
            Ok(r) => r,
            Err(why) => return Change::Failed(format!("the build's lift.json doesn't read: {why}")),
        };
        self.values = report.literals.iter().map(|l| l.value).collect();
        self.report = report;
        self.built = self.seen.clone();
        self.version = version;
        Change::Built { version, dir }
    }

    /// The build's literals' values in the files as they are (`now`), if what changed since
    /// the build is literals alone: each lifted file's tokens are the build's but for numbers,
    /// and every other file is as the build saw it.
    fn literal_values(&self, now: &Files) -> Option<Vec<f32>> {
        if !now.keys().eq(self.built.keys()) {
            return None;
        }
        let canonical: BTreeMap<PathBuf, &PathBuf> =
            now.keys().filter_map(|p| Some((p.canonicalize().ok()?, p))).collect();
        let mut texts = Vec::new();
        let mut lifted = Vec::new();
        for f in &self.report.files {
            let path = *canonical.get(&self.pkg.join(&f.path).canonicalize().ok()?)?;
            let current = &now[path].text;
            if !same_but_numbers(&f.text, current) {
                return None;
            }
            texts.push((Numbers::new(f.text.clone()), Numbers::new(current.clone())));
            lifted.push(path);
        }
        if now.iter().any(|(p, s)| !lifted.contains(&p) && self.built[p].text != s.text) {
            return None;
        }
        let mut values = Vec::with_capacity(self.report.literals.len());
        for l in &self.report.literals {
            let (built, current) = texts.get(l.file as usize)?;
            let (s, e) = moved(built, current, l.start, l.end)?;
            values.push(crate::edit::signed_value(&current.text()[s as usize..e as usize])?);
        }
        Some(values)
    }
}

// ---- the server ---------------------------------------------------------------------------------

/// A change as the page gets it, numbered.
struct Event {
    seq: u64,
    json: serde_json::Value,
    /// When the watcher saw it, and when its files were saved.
    seen: Instant,
    saved: Option<SystemTime>,
}

/// How long after its save a change was first on the screen, as the page reported it.
#[derive(Clone, Debug)]
pub struct Shown {
    pub seq: u64,
    /// `literals`, `build` or `error`.
    pub kind: String,
    /// The page's frame that showed it.
    pub frame: u64,
    /// From the save (the changed file's time) to the report, and from the watcher seeing the
    /// change to the report.
    pub since_save: Option<Duration>,
    pub since_seen: Duration,
}

struct State {
    out: PathBuf,
    port: u16,
    live: Mutex<Live>,
    changed: Condvar,
}

struct Live {
    version: u64,
    events: Vec<Event>,
    shown: Vec<Shown>,
}

/// A running `wrela run` server: its port, and its threads, which stop when it's dropped.
pub struct Server {
    pub port: u16,
    /// Where the builds are (`<n>/`), and test mode's results (`results/`).
    pub out: PathBuf,
    state: Arc<State>,
    stopping: Arc<AtomicBool>,
}

impl Server {
    /// Serves until the process ends.
    pub fn wait(self) {
        loop {
            std::thread::park();
        }
    }

    /// Each change the page has shown so far.
    pub fn shown(&self) -> Vec<Shown> {
        self.state.live.lock().expect("the state").shown.clone()
    }

    /// The changes so far, as the page gets them.
    pub fn changes(&self) -> Vec<serde_json::Value> {
        self.state.live.lock().expect("the state").events.iter().map(|e| e.json.clone()).collect()
    }

    pub fn stop(&self) {
        if !self.stopping.swap(true, Ordering::SeqCst) {
            self.state.changed.notify_all();
            // Wakes the accepting thread, which then sees it's stopping.
            let _ = TcpStream::connect(("127.0.0.1", self.port));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Builds the program at `pkg` lifted (`lift`, by default its own package) into `out`, serves
/// it on 127.0.0.1:`port` (0: a free port), and watches its files: the server, or why it
/// couldn't start. `quiet`: print nothing.
pub fn serve(
    pkg: &Path,
    out: &Path,
    port: u16,
    lift: &[String],
    debug: bool,
    quiet: bool,
) -> Result<Server, String> {
    let _ = std::fs::remove_dir_all(out);
    let mut watcher = Watcher::start(pkg, out, lift, debug)?;
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("can't listen on 127.0.0.1:{port}: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let state = Arc::new(State {
        out: out.to_path_buf(),
        port,
        live: Mutex::new(Live { version: watcher.version(), events: Vec::new(), shown: Vec::new() }),
        changed: Condvar::new(),
    });
    let stopping = Arc::new(AtomicBool::new(false));
    let (watching, stop_watching) = (Arc::clone(&state), Arc::clone(&stopping));
    std::thread::spawn(move || {
        while !stop_watching.load(Ordering::SeqCst) {
            std::thread::sleep(SCAN);
            let Some(change) = watcher.poll() else { continue };
            let seen = Instant::now();
            let json = match &change {
                Change::Literals(values) => {
                    if !quiet {
                        println!("{} literal(s) changed", values.len());
                    }
                    serde_json::json!({ "kind": "literals", "values": values })
                }
                Change::Built { version, .. } => {
                    if !quiet {
                        println!("built version {version}");
                    }
                    serde_json::json!({ "kind": "build", "base": format!("/{version}/") })
                }
                Change::Failed(why) => {
                    if !quiet {
                        eprint!("{why}");
                    }
                    serde_json::json!({ "kind": "error", "text": why })
                }
            };
            let mut s = watching.live.lock().expect("the state");
            if let Change::Built { version, .. } = change {
                s.version = version;
            }
            let seq = s.events.len() as u64 + 1;
            let mut json = json;
            json["seq"] = seq.into();
            s.events.push(Event { seq, json, seen, saved: watcher.saved });
            watching.changed.notify_all();
        }
    });
    let (accepting, stop_accepting) = (Arc::clone(&state), Arc::clone(&stopping));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if stop_accepting.load(Ordering::SeqCst) {
                break;
            }
            let (state, stopping) = (Arc::clone(&accepting), Arc::clone(&stop_accepting));
            std::thread::spawn(move || {
                let _ = handle(stream, &state, &stopping, quiet);
            });
        }
    });
    Ok(Server { port, out: out.to_path_buf(), state, stopping })
}

/// What `/` and a build's `index.html` get: the mark that tells the runtime the page is live.
const MARK: &str = r#"<meta name="wrela-live" content="1">"#;

/// How long a long poll waits for a change before it answers that there's none.
const LONG_POLL: Duration = Duration::from_secs(15);

fn handle(stream: TcpStream, state: &State, stopping: &AtomicBool, quiet: bool) -> std::io::Result<()> {
    let Some(req) = http::read(&stream, state.port)? else { return Ok(()) };
    let mut out = stream;
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/live/next") => {
            let after: u64 = req.param("after").and_then(|v| v.parse().ok()).unwrap_or(0);
            let deadline = Instant::now() + LONG_POLL;
            let mut s = state.live.lock().expect("the state");
            while s.events.len() as u64 <= after && !stopping.load(Ordering::SeqCst) {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                s = state.changed.wait_timeout(s, left).expect("the state").0;
            }
            let events: Vec<&serde_json::Value> =
                s.events.iter().filter(|e| e.seq > after).map(|e| &e.json).collect();
            let body = serde_json::to_vec(&events).unwrap_or_default();
            drop(s);
            http::respond(&mut out, 200, "application/json", &body)
        }
        ("POST", "/live/shown") => {
            let v: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let (Some(seq), Some(frame)) = (v["seq"].as_u64(), v["frame"].as_u64()) else {
                return http::respond(&mut out, 403, "text/plain", b"a report is {seq, frame}");
            };
            let mut s = state.live.lock().expect("the state");
            if let Some(e) = s.events.iter().find(|e| e.seq == seq) {
                let shown = Shown {
                    seq,
                    kind: e.json["kind"].as_str().unwrap_or("").to_string(),
                    frame,
                    since_save: e.saved.and_then(|t| SystemTime::now().duration_since(t).ok()),
                    since_seen: e.seen.elapsed(),
                };
                if !quiet {
                    let save = shown.since_save.map_or(String::new(), |d| {
                        format!("{:.0} ms after the save, ", d.as_secs_f64() * 1000.0)
                    });
                    println!("shown at frame {frame}: {save}{:.0} ms after the watcher saw it", shown.since_seen.as_secs_f64() * 1000.0);
                }
                s.shown.push(shown);
            }
            http::respond(&mut out, 200, "text/plain", b"")
        }
        ("PUT", path) if path.starts_with("/results/") => {
            http::put_result(&mut out, &state.out.join("results"), &path["/results/".len()..], &req.body)
        }
        ("GET", path) => {
            // `/<version>/file` is that build's; any other path, the newest build's.
            let trimmed = path.trim_start_matches('/');
            let version = state.live.lock().expect("the state").version;
            let (dir, rel) = match trimmed.split_once('/') {
                Some((v, rest)) if v.parse::<u64>().is_ok() => (state.out.join(v), rest),
                _ => (state.out.join(version.to_string()), trimmed),
            };
            let rel = if rel.is_empty() { "index.html" } else { rel };
            if wrela_abi::check::path_problem(rel).is_some() {
                return http::respond(&mut out, 404, "text/plain", b"not found");
            }
            match std::fs::read(dir.join(rel)) {
                Ok(mut bytes) => {
                    if rel == "index.html" {
                        let html = String::from_utf8_lossy(&bytes).replacen("<head>", &format!("<head>{MARK}"), 1);
                        bytes = html.into_bytes();
                    }
                    http::respond(&mut out, 200, http::mime(rel), &bytes)
                }
                Err(_) => http::respond(&mut out, 404, "text/plain", b"not found"),
            }
        }
        _ => http::respond(&mut out, 405, "text/plain", b"not allowed"),
    }
}
