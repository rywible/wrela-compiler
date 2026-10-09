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
//! The server's paths: `/` and the build's files (the newest build), `/<n>/...` (build `n`),
//! `/live/next?after=<seq>` (a long poll: the changes after `seq`, as JSON), `/live/shown` (the
//! page says which frame first showed a change), `/results/<file>` (test mode's results).

use crate::http;
use crate::lift::{Report, ReportLiteral};
use crate::studio::{Numbers, lifted_files, literal_values};
use std::collections::BTreeMap;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// How often the watcher looks at the files.
pub const SCAN: Duration = Duration::from_millis(20);

/// A file as a watcher saw it: when it was last changed, its length, and its text.
#[derive(Clone, Debug)]
pub(crate) struct Seen {
    modified: SystemTime,
    len: u64,
    /// Shared with the scans after, until the file is read again.
    pub text: Arc<str>,
}

impl Seen {
    /// Whether `other`'s text is this one's: the same text, unless one of them was read again.
    pub fn same_text(&self, other: &Seen) -> bool {
        Arc::ptr_eq(&self.text, &other.text) || self.text == other.text
    }
}

/// Watched files as a scan found them, by path.
pub(crate) type Files = BTreeMap<PathBuf, Seen>;

/// The source files (modules and manifests, not build output) of the packages in `dirs`, each
/// read again only where its time or length has changed since `before`.
pub(crate) fn scan(dirs: &[PathBuf], before: &Files) -> Files {
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
                    _ => Seen {
                        modified,
                        len,
                        text: std::fs::read_to_string(&path).unwrap_or_default().into(),
                    },
                };
                out.insert(path, seen);
            }
        }
    }
    let mut out = Files::new();
    for d in dirs {
        walk(d, true, before, &mut out);
    }
    out
}

/// The files whose texts differ between `before` and `now`, and those only one of them has.
pub(crate) fn changed<'a>(before: &'a Files, now: &'a Files) -> impl Iterator<Item = &'a PathBuf> {
    now.iter()
        .filter(|(p, s)| before.get(*p).is_none_or(|old| !old.same_text(s)))
        .map(|(p, _)| p)
        .chain(before.keys().filter(|p| !now.contains_key(*p)))
}

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

/// A program, built lifted, and its files watched (see the module's docs).
pub struct Watcher {
    pkg: PathBuf,
    lift: Vec<String>,
    kind: crate::BuildKind,
    /// Where the builds go: `out/<version>`.
    out: PathBuf,
    version: u64,
    /// The packages' directories.
    dirs: Vec<PathBuf>,
    /// The files when last looked at, and as the newest build saw them.
    seen: Files,
    built: Files,
    /// The newest build's literals, and the files it lifted as it saw them ([`lifted_files`]):
    /// `None` if one of those isn't among the files watched.
    literals: Vec<ReportLiteral>,
    lifted: Option<Vec<(PathBuf, Numbers)>>,
    /// Each literal's value now: the build's, then each change's.
    values: Vec<f32>,
    /// When the newest change was saved: the newest time a changed file was modified.
    pub saved: Option<SystemTime>,
}

impl Watcher {
    /// Builds the program at `pkg` lifted (the packages `lift` names, by default the program's
    /// own), as `kind` says, into `out/1`, and starts watching its files.
    pub fn start(
        pkg: &Path,
        out: &Path,
        lift: &[String],
        kind: crate::BuildKind,
    ) -> Result<Watcher, String> {
        let packages = crate::packages(pkg)?;
        let lift = if lift.is_empty() { vec![packages[0].0.clone()] } else { lift.to_vec() };
        let dirs: Vec<PathBuf> = packages.into_iter().map(|(_, d)| d).collect();
        let mut w = Watcher {
            pkg: pkg.to_path_buf(),
            lift,
            kind,
            out: out.to_path_buf(),
            version: 0,
            seen: scan(&dirs, &Files::new()),
            dirs,
            built: Files::new(),
            literals: Vec::new(),
            lifted: None,
            values: Vec::new(),
            saved: None,
        };
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

    /// Looks at the files once: what's changed since the last look, if anything.
    pub fn poll(&mut self) -> Option<Change> {
        let now = scan(&self.dirs, &self.seen);
        let changed: Vec<&PathBuf> = changed(&self.seen, &now).collect();
        if changed.is_empty() {
            self.seen = now;
            return None;
        }
        self.saved = changed.iter().filter_map(|p| now.get(*p)).map(|s| s.modified).max();
        let values = (self.lifted.as_deref())
            .and_then(|lifted| literal_values(lifted, &self.literals, &now, &self.built));
        self.seen = now;
        let Some(values) = values else { return Some(self.build()) };
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

    /// Builds the program as its files are now, into the next version's directory.
    fn build(&mut self) -> Change {
        let out = match crate::build_lifted(&self.pkg, &self.lift, self.kind) {
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
        let report = match out.files.iter().find(|(p, _)| p == "lift.json") {
            Some((_, json)) => serde_json::from_slice::<Report>(json).map_err(|e| e.to_string()),
            None => Err("the build has none".to_string()),
        };
        let report = match report {
            Ok(r) => r,
            Err(why) => {
                return Change::Failed(format!("the build's lift.json doesn't read: {why}"));
            }
        };
        self.values = report.literals.iter().map(|l| l.value).collect();
        self.lifted = lifted_files(&self.pkg, &report, &self.seen);
        self.literals = report.literals;
        self.built = self.seen.clone();
        self.version = version;
        Change::Built { version, dir }
    }
}

// ---- the server ---------------------------------------------------------------------------------

/// A change as the page gets it, numbered (its `seq`, from 1: its place in [`Live::events`]).
struct Event {
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
        if http::stop(self.port, &self.stopping) {
            // Long polls end too.
            self.state.changed.notify_all();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Builds the program at `pkg` lifted (`lift`, by default its own package), as `kind` says,
/// into `out`, serves it on 127.0.0.1:`port` (0: a free port), and watches its files: the
/// server, or why it couldn't start. `quiet`: print nothing.
pub fn serve(
    pkg: &Path,
    out: &Path,
    port: u16,
    lift: &[String],
    kind: crate::BuildKind,
    quiet: bool,
) -> Result<Server, String> {
    let _ = std::fs::remove_dir_all(out);
    let mut watcher = Watcher::start(pkg, out, lift, kind)?;
    let (listener, port) = http::listen(port)?;
    let state = Arc::new(State {
        out: out.to_path_buf(),
        port,
        live: Mutex::new(Live {
            version: watcher.version(),
            events: Vec::new(),
            shown: Vec::new(),
        }),
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
            let mut json = json;
            json["seq"] = (s.events.len() as u64 + 1).into();
            s.events.push(Event { json, seen, saved: watcher.saved });
            watching.changed.notify_all();
        }
    });
    let (accepting, stop_accepting) = (Arc::clone(&state), Arc::clone(&stopping));
    http::accept(listener, Arc::clone(&stopping), move |stream| {
        let _ = handle(stream, &accepting, &stop_accepting, quiet);
    });
    Ok(Server { port, state, stopping })
}

/// What `/` and a build's `index.html` get: the mark that tells the runtime the page is live.
const MARK: &str = r#"<meta name="wrela-live" content="1">"#;

/// How long a long poll waits for a change before it answers that there's none.
const LONG_POLL: Duration = Duration::from_secs(15);

fn handle(
    stream: TcpStream,
    state: &State,
    stopping: &AtomicBool,
    quiet: bool,
) -> std::io::Result<()> {
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
            let after = usize::try_from(after).unwrap_or(usize::MAX);
            let events: Vec<&serde_json::Value> =
                s.events.iter().skip(after).map(|e| &e.json).collect();
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
            let event = seq.checked_sub(1).and_then(|i| s.events.get(usize::try_from(i).ok()?));
            if let Some(e) = event {
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
                    println!(
                        "shown at frame {frame}: {save}{:.0} ms after the watcher saw it",
                        shown.since_seen.as_secs_f64() * 1000.0
                    );
                }
                s.shown.push(shown);
            }
            http::respond(&mut out, 200, "text/plain", b"")
        }
        ("PUT", path) if path.starts_with("/results/") => http::put_result(
            &mut out,
            &state.out.join("results"),
            &path["/results/".len()..],
            &req.body,
        ),
        ("GET", path) => {
            // `/<version>/file` is that build's; any other path, the newest build's.
            let trimmed = path.trim_start_matches('/');
            let version = state.live.lock().expect("the state").version;
            let (dir, rel) = match trimmed.split_once('/') {
                Some((v, rest)) if v.parse::<u64>().is_ok() => (state.out.join(v), rest),
                _ => (state.out.join(version.to_string()), trimmed),
            };
            let rel = if rel.is_empty() { "index.html" } else { rel };
            http::get_file(&mut out, &dir, rel, |html| {
                html.replacen("<head>", &format!("<head>{MARK}"), 1)
            })
        }
        _ => http::respond(&mut out, 405, "text/plain", b"not allowed"),
    }
}
