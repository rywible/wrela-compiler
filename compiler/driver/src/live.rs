//! Hot reload (#28 §9, language.md §23): `wrela run <package>` builds the program lifted, serves
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
use crate::lift::{Numbers, lifted_files, literal_values};
use crate::lift::{Report, ReportLiteral};
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
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() {
                if !(top && wrela_sema::OUTPUT_DIRS.contains(&name.as_str())) {
                    walk(&path, false, before, out);
                }
            } else if name.ends_with(".wrela") || name == "wrela.toml" {
                see(path, before, out);
            }
        }
    }
    let mut out = Files::new();
    for d in dirs {
        walk(d, true, before, &mut out);
    }
    out
}

/// Adds the file at `path` to `out` if it's a regular file, read again only where its time or
/// length has changed since `before`.
fn see(path: PathBuf, before: &Files, out: &mut Files) {
    // Reading follows a file symlink, so its cache key must describe the target too. A walk
    // uses the entry's own metadata to avoid following directory symlinks.
    let Ok(meta) = std::fs::metadata(&path) else { return };
    if !meta.is_file() {
        return;
    }
    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let len = meta.len();
    let seen = match before.get(&path) {
        Some(s) if s.modified == modified && s.len == len => s.clone(),
        _ => {
            Seen { modified, len, text: std::fs::read_to_string(&path).unwrap_or_default().into() }
        }
    };
    out.insert(path, seen);
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
    /// Where the builds go: `out/<version>`, the last two kept.
    out: PathBuf,
    version: u64,
    /// The packages' directories, and the manifests of the workspaces they were found through
    /// (watched alone: a workspace's directory holds far more than the program's files).
    dirs: Vec<PathBuf>,
    workspaces: Vec<PathBuf>,
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
        check_output_path(pkg, out)?;
        let inputs = crate::inputs(pkg)?;
        let lift = if lift.is_empty() { vec![inputs.packages[0].0.clone()] } else { lift.to_vec() };
        let dirs: Vec<PathBuf> = inputs.packages.into_iter().map(|(_, d)| d).collect();
        let mut w = Watcher {
            pkg: pkg.to_path_buf(),
            lift,
            kind,
            out: out.to_path_buf(),
            version: 0,
            seen: Files::new(),
            dirs,
            workspaces: inputs.workspaces,
            built: Files::new(),
            literals: Vec::new(),
            lifted: None,
            values: Vec::new(),
            saved: None,
        };
        w.seen = w.look(&Files::new());
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

    /// The files watched as they are now, each read again only where it changed since `before`.
    fn look(&self, before: &Files) -> Files {
        let mut now = scan(&self.dirs, before);
        for path in &self.workspaces {
            see(path.clone(), before, &mut now);
        }
        now
    }

    /// Looks at the files once: what's changed since the last look, if anything.
    pub fn poll(&mut self) -> Option<Change> {
        let now = self.look(&self.seen);
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
        if let Ok(inputs) = crate::inputs(&self.pkg) {
            self.dirs = inputs.packages.into_iter().map(|(_, dir)| dir).collect();
            self.workspaces = inputs.workspaces;
            self.seen = self.look(&self.seen);
        }
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
        // The build before the last goes: a page still swapping to the last one fetches its
        // files, and nothing reads an older one (a session's builds would fill the disk).
        if version > 2 {
            let _ = std::fs::remove_dir_all(self.out.join((version - 2).to_string()));
        }
        Change::Built { version, dir }
    }
}

// ---- the server ---------------------------------------------------------------------------------

/// The browser runtime's scripts a build holds, the same in every build.
const RUNTIME_SCRIPTS: [&str; 3] = ["main.js", "worker.js", "worklet.js"];

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
    /// How many long polls pages have made: a page asks as soon as it runs.
    polls: u64,
}

impl Live {
    /// The number (`seq`) of the change that made build `version`; 0 for the first build. A
    /// page of that build holds every change up to it.
    fn built_at(&self, version: u64) -> u64 {
        let base = format!("/{version}/");
        self.events
            .iter()
            .position(|e| e.json["kind"] == "build" && e.json["base"] == base.as_str())
            .map_or(0, |i| i as u64 + 1)
    }
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

    /// How many times pages have asked for changes (`/live/next`): one is running once it's
    /// asked (a test edits after that, so the page sees its edits as changes, not as part of
    /// the build it loaded).
    pub fn polls(&self) -> u64 {
        self.state.live.lock().expect("the state").polls
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
/// server, or why it couldn't start. `quiet`: print nothing. `built` is given each build's
/// directory before any page hears of it, the first build's too.
pub fn serve(
    pkg: &Path,
    out: &Path,
    port: u16,
    lift: &[String],
    kind: crate::BuildKind,
    quiet: bool,
    mut built: impl FnMut(&Path) + Send + 'static,
) -> Result<Server, String> {
    check_output_path(pkg, out)?;
    let _ = std::fs::remove_dir_all(out);
    let mut watcher = Watcher::start(pkg, out, lift, kind)?;
    built(&watcher.dir());
    let (listener, port) = http::listen(port)?;
    let state = Arc::new(State {
        out: out.to_path_buf(),
        port,
        live: Mutex::new(Live {
            version: watcher.version(),
            events: Vec::new(),
            shown: Vec::new(),
            polls: 0,
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
            let mut json = match &change {
                Change::Literals(values) => {
                    if !quiet {
                        println!("{} literal(s) changed", values.len());
                    }
                    serde_json::json!({ "kind": "literals", "values": values })
                }
                Change::Built { version, dir } => {
                    built(dir);
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

/// A live build owned below `<pkg>/build` must not escape through a link in that path. Other
/// callers may choose an unrelated output directory; its final component is still never a link.
fn check_output_path(pkg: &Path, out: &Path) -> Result<(), String> {
    if let (Ok(package), Ok(output)) = (pkg.canonicalize(), out.canonicalize())
        && package.starts_with(&output)
    {
        return Err(format!(
            "the live output {} contains the source package {}; choose a directory of its own",
            out.display(),
            pkg.display()
        ));
    }
    let build = pkg.join("build");
    let root = if out.starts_with(&build) { build.as_path() } else { out };
    crate::ensure_no_symlink_path(root, out)
        .map_err(|e| format!("can't use {}: {e}", out.display()))
}

/// What `/` and a build's `index.html` get, which marks a served page as live: the changes it
/// already holds (the newest build's, and the literals before it), after which it asks for more,
/// and the build's directory, which its files are fetched from. A page opened after hot reloads
/// starts from the newest build, not from the server's first change (playing spike 17: it
/// replayed old builds' changes and asked for a build the server had deleted,
/// `/2/manifest.json`, 404).
fn mark(seq: u64, version: u64) -> String {
    format!(r#"<meta name="wrela-live" content="{seq}"><base href="/{version}/">"#)
}

/// The file a PUT of a page's results names: `/results/<file>`, or `/<version>/results/<file>`.
fn results_file(path: &str) -> Option<&str> {
    let rest = path.strip_prefix('/')?;
    let rest = match rest.split_once('/') {
        Some((v, r)) if v.parse::<u64>().is_ok() => r,
        _ => rest,
    };
    rest.strip_prefix("results/")
}

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
            s.polls += 1;
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
        // A page's results, from the server's root or (a page pinned to its build's directory,
        // `mark`) the build's.
        ("PUT", path) if results_file(path).is_some() => http::put_result(
            &mut out,
            &state.out,
            results_file(path).unwrap_or_default(),
            &req.body,
        ),
        ("GET", path) => {
            // `/<version>/file` is that build's; any other path, the newest build's. A page is
            // marked with the changes its build holds (`mark`), read with the build's number.
            let trimmed = path.trim_start_matches('/');
            let (version, seq) = {
                let s = state.live.lock().expect("the state");
                let version = s.version;
                // The newest build includes the changes through the event that made it, but
                // not literal-only edits made since. A newly opened page must replay those.
                (version, s.built_at(version))
            };
            let built =
                trimmed.split_once('/').and_then(|(v, rest)| Some((v.parse::<u64>().ok()?, rest)));
            let (dir, rel, version, seq) = match built {
                // The runtime's own scripts are the same in every build, and a page's threads
                // start from the script its render worker came from: a build the server has
                // since deleted (it keeps two) still gives them, from the newest. (A page open
                // through three hot reloads stopped: its next ticker's script was gone.)
                Some((_, rest)) if RUNTIME_SCRIPTS.contains(&rest) => {
                    (state.out.join(version.to_string()), rest, version, seq)
                }
                Some((v, rest)) => {
                    // The change that made build `v`, or none (the first build).
                    let made = state.live.lock().expect("the state").built_at(v);
                    (state.out.join(v.to_string()), rest, v, made)
                }
                None => (state.out.join(version.to_string()), trimmed, version, seq),
            };
            let rel = if rel.is_empty() { "index.html" } else { rel };
            http::get_file(&mut out, &dir, rel, |html| {
                html.replacen("<head>", &format!("<head>{}", mark(seq, version)), 1)
            })
        }
        _ => http::respond(&mut out, 405, "text/plain", b"not allowed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_output_cannot_contain_the_source_package() {
        let root = std::env::temp_dir().join(format!("wrela-live-output-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let pkg = root.join("program");
        std::fs::create_dir_all(&pkg).expect("package");
        for out in [&root, &pkg] {
            assert!(
                check_output_path(&pkg, out).is_err(),
                "starting the server would delete its package at {}",
                out.display()
            );
        }
        assert!(check_output_path(&pkg, &pkg.join("build/live")).is_ok());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A workspace's manifest is watched, alone: a change to its members can change which
    /// package a dependency is, and the rebuild then watches that package's files.
    #[test]
    fn a_rebuild_follows_the_workspaces_members() {
        let root = std::env::temp_dir().join(format!("wrela-live-members-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |path: &str, text: &str| {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().expect("a parent")).expect("directory");
            std::fs::write(p, text).expect("write");
        };
        write("wrela.toml", "[workspace]\nmembers = [\"program\", \"a\"]\n");
        write(
            "program/wrela.toml",
            "[package]\nname = \"program\"\n\n[dependencies]\nlibrary = { workspace = true }\n",
        );
        write(
            "program/main.wrela",
            "fn v() -> f32 {\n    library::value::value()\n}\n\npub fn frame(time: f32, width: u32, height: u32) {}\n",
        );
        for dir in ["a", "lib-b"] {
            write(&format!("{dir}/wrela.toml"), "[package]\nname = \"library\"\n");
            write(&format!("{dir}/value.wrela"), "pub fn value() -> f32 { 0.5 }\n");
        }
        let pkg = root.join("program");
        let mut watcher =
            Watcher::start(&pkg, &pkg.join("build/live"), &[], crate::BuildKind::default())
                .expect("watcher");
        let (a, b) =
            (root.join("a").canonicalize().unwrap(), root.join("lib-b").canonicalize().unwrap());
        assert!(watcher.dirs.contains(&a) && !watcher.dirs.contains(&b));
        write("wrela.toml", "[workspace]\nmembers = [\"program\", \"lib-b\"]\n");
        assert!(
            matches!(watcher.poll(), Some(Change::Built { .. })),
            "a change to the workspace's members must rebuild"
        );
        assert!(watcher.dirs.contains(&b) && !watcher.dirs.contains(&a), "{:?}", watcher.dirs);
        write("lib-b/value.wrela", "pub fn value() -> f32 { 0.75 }\n");
        assert!(
            matches!(watcher.poll(), Some(Change::Built { .. })),
            "the new member's edit must rebuild"
        );
        write("a/value.wrela", "pub fn value() -> f32 { 0.25 }\n");
        assert_eq!(watcher.poll(), None, "the old member isn't watched");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_rebuild_watches_new_dependencies() {
        let root = std::env::temp_dir().join(format!("wrela-live-new-dep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let pkg = root.join("program");
        let dep = root.join("library");
        std::fs::create_dir_all(&pkg).expect("program directory");
        std::fs::create_dir_all(&dep).expect("dependency directory");
        std::fs::write(
            pkg.join("main.wrela"),
            "pub fn frame(time: f32, width: u32, height: u32) {}\n",
        )
        .expect("program");
        std::fs::write(dep.join("value.wrela"), "pub fn value() -> f32 { 0.5 }\n")
            .expect("dependency");
        let mut watcher =
            Watcher::start(&pkg, &pkg.join("build/live"), &[], crate::BuildKind::default())
                .expect("watcher");
        std::fs::write(
            pkg.join("wrela.toml"),
            "[dependencies]\nlibrary = { path = \"../library\" }\n",
        )
        .expect("new dependency");
        assert!(matches!(watcher.poll(), Some(Change::Built { .. })));
        std::fs::write(dep.join("value.wrela"), "pub fn value() -> f32 { 0.75 }\n")
            .expect("dependency edit");
        assert!(
            matches!(watcher.poll(), Some(Change::Built { .. })),
            "the dependency edit must rebuild"
        );
        let _ = std::fs::remove_dir_all(root);
    }
    /// Each build's directory is given to `serve`'s `built` before any page hears of the build
    /// (so a file written there, as `wrela run` writes a program's `net.json`, is there when the
    /// page asks), the first build's too.
    #[test]
    fn each_build_is_given_to_built_before_a_page_hears_of_it() {
        let root = std::env::temp_dir().join(format!("wrela-live-built-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let pkg = root.join("program");
        std::fs::create_dir_all(&pkg).expect("program directory");
        // Each version has another function: an edit of the code, not only of its literals.
        let frame = |n: usize| {
            format!("pub fn frame(time: f32, width: u32, height: u32) {{}}\nfn f{n}() {{}}\n")
        };
        std::fs::write(pkg.join("wrela.toml"), "[package]\nname = \"program\"\n")
            .expect("manifest");
        std::fs::write(pkg.join("main.wrela"), frame(1)).expect("program");
        let built = |dir: &Path| std::fs::write(dir.join("marked"), b"").expect("mark");
        let out = root.join("out");
        let server =
            serve(&pkg, &out, 0, &[], crate::BuildKind::default(), true, built).expect("serve");
        assert!(out.join("1/marked").exists(), "the first build is given to `built`");
        std::fs::write(pkg.join("main.wrela"), frame(2)).expect("edit");
        let began = Instant::now();
        while !server.changes().iter().any(|c| c["kind"] == "build") {
            assert!(
                began.elapsed() < Duration::from_secs(20),
                "the edit wasn't built: {:?}",
                server.changes()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(out.join("2/marked").exists(), "a build is given to `built` before it's shown");
        server.stop();
        let _ = std::fs::remove_dir_all(root);
    }

    use std::io::{Read, Write};

    /// A page opened after a literal-only edit loads the newest build, then receives that edit:
    /// the build itself doesn't already hold it.
    #[test]
    fn a_new_page_replays_literals_changed_since_its_build() {
        let out = std::env::temp_dir().join(format!("wrela-live-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(out.join("1")).expect("build directory");
        std::fs::write(out.join("1/index.html"), "<html><head></head></html>").expect("index.html");

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("listen");
        let port = listener.local_addr().expect("address").port();
        let state = State {
            out: out.clone(),
            port,
            live: Mutex::new(Live {
                version: 1,
                events: vec![Event {
                    json: serde_json::json!({
                        "seq": 1,
                        "kind": "literals",
                        "values": [[0, 0.75]],
                    }),
                    seen: Instant::now(),
                    saved: None,
                }],
                shown: Vec::new(),
                polls: 0,
            }),
            changed: Condvar::new(),
        };
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
            write!(stream, "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").expect("request");
            let mut response = String::new();
            stream.read_to_string(&mut response).expect("response");
            response
        });
        let (stream, _) = listener.accept().expect("accept");
        handle(stream, &state, &AtomicBool::new(false), true).expect("serve the page");
        let response = client.join().expect("client");
        assert!(
            response.contains(r#"<meta name="wrela-live" content="0"><base href="/1/">"#),
            "{response}"
        );
        let _ = std::fs::remove_dir_all(out);
    }

    /// Source-file symlinks are package modules. A scan keys their cached contents by the
    /// target's metadata, since that is also where `read_to_string` gets their contents.
    #[cfg(unix)]
    #[test]
    fn scan_sees_a_symlinked_source_targets_change() {
        let d = std::env::temp_dir().join(format!("wrela-live-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        let target = d.join(".main-target");
        let link = d.join("main.wrela");
        std::fs::write(&target, "first\n").expect("write target");
        std::os::unix::fs::symlink(&target, &link).expect("link");

        let first = scan(std::slice::from_ref(&d), &Files::new());
        assert_eq!(first[&link].text.as_ref(), "first\n");
        std::fs::write(&target, "second, and longer\n").expect("rewrite target");
        let second = scan(std::slice::from_ref(&d), &first);

        assert_eq!(second[&link].text.as_ref(), "second, and longer\n");
        assert_eq!(changed(&first, &second).collect::<Vec<_>>(), [&link]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_follow_a_symlinked_directory() {
        let d = std::env::temp_dir().join(format!("wrela-live-dir-link-{}", std::process::id()));
        let outside =
            std::env::temp_dir().join(format!("wrela-live-dir-outside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&d).expect("package");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(d.join("main.wrela"), "").expect("source");
        std::fs::write(outside.join("other.wrela"), "").expect("outside source");
        std::os::unix::fs::symlink(&outside, d.join("linked")).expect("directory symlink");

        let found = scan(std::slice::from_ref(&d), &Files::new());
        assert_eq!(found.len(), 1);
        assert!(found.contains_key(&d.join("main.wrela")));
        let _ = std::fs::remove_dir_all(d);
        let _ = std::fs::remove_dir_all(outside);
    }
}
