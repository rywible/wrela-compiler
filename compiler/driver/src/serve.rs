//! The studio server (AC10 of #39): serves the lens's build to Chrome on 127.0.0.1 only, takes
//! the lens's edits (a `std::io::post` to `/studio/edit`) and writes them through `wrela edit`,
//! answers its fit's request for the reference image (`/studio/reference`, from `--reference`),
//! and watches the subject's files. A change that touches only literals (another tool's edit)
//! goes to the open lens as new values, which it sets with no rebuild; any other change
//! rebuilds the lens, and the page reloads.
//!
//! The page is the build's, with a script that asks the server what's new every 250 ms:
//! `/studio/state?since=n` answers the build's version and the literal updates since `n`, which
//! the script types into the lens as its `set` command.
//!
//! It answers only requests addressed to this machine by name (so a page elsewhere can't reach
//! it through DNS rebinding), and takes posts only from its own pages (their `Origin`), so
//! another site open in the browser can't edit the files. Test mode's results (`PUT
//! /results/<file>`) go to the page's `results/`.

use crate::live::{Files, changed, scan};
use crate::{http, studio};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the server knows, shared by its threads: what it was started with, and what changes.
struct Shared {
    pkg: PathBuf,
    page: PathBuf,
    debug: bool,
    /// The fit's reference (`--reference`), as the lens takes it.
    reference: Option<Vec<u8>>,
    /// The port it answers on.
    port: u16,
    live: Mutex<Live>,
}

/// What changes as the server runs.
struct Live {
    /// The build the page has: it reloads when this changes.
    version: u64,
    /// The literals the lens should set, in order: (literal, value).
    updates: Vec<(u32, f32)>,
    /// The values the lens has, by literal: the build's, then each update's.
    values: Vec<f32>,
    /// Why the last rebuild failed, if it did.
    error: Option<String>,
}

/// A running studio server: its port, and its threads, which stop when it's dropped (or
/// `stop`ped).
pub struct Server {
    pub port: u16,
    stopping: Arc<AtomicBool>,
}

impl Server {
    /// Serves until the process ends.
    pub fn wait(self) {
        loop {
            std::thread::park();
        }
    }

    pub fn stop(&self) {
        http::stop(self.port, &self.stopping);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts serving the lens's build at `page` (for the subject at `pkg`) on 127.0.0.1:`port`
/// (0: a free port), and watching the subject's files: the server, or why it couldn't listen.
/// `reference` answers the fit's request for its reference image.
pub fn start(
    pkg: &Path,
    page: &Path,
    port: u16,
    debug: bool,
    reference: Option<Vec<u8>>,
) -> Result<Server, String> {
    let (listener, port) = http::listen(port)?;
    let state = Arc::new(Shared {
        pkg: pkg.to_path_buf(),
        page: page.to_path_buf(),
        debug,
        reference,
        port,
        live: Mutex::new(Live {
            version: 1,
            updates: Vec::new(),
            values: build_values(page),
            error: None,
        }),
    });
    let stopping = Arc::new(AtomicBool::new(false));
    let seen = scan(std::slice::from_ref(&state.pkg), &Files::new());
    let (watcher, stop_watching) = (Arc::clone(&state), Arc::clone(&stopping));
    std::thread::spawn(move || watch(&watcher, &stop_watching, seen));
    http::accept(listener, Arc::clone(&stopping), move |stream| {
        let _ = handle(stream, &state);
    });
    Ok(Server { port, stopping })
}

/// The values the build gives its literals.
fn build_values(page: &Path) -> Vec<f32> {
    studio::report(page).map(|r| r.literals.iter().map(|l| l.value).collect()).unwrap_or_default()
}

/// Every 200 ms: what changed in the subject's files (its modules and manifests, not its build
/// output) since `seen`. Literals only: new values for the lens. Anything else: a rebuild, and
/// a new version.
fn watch(state: &Shared, stopping: &AtomicBool, mut seen: Files) {
    // The files as the lens's build saw them: as the server first found them (the build is of
    // what was there when it started), then as each rebuild found them.
    let mut built = seen.clone();
    loop {
        std::thread::sleep(Duration::from_millis(200));
        if stopping.load(Ordering::SeqCst) {
            return;
        }
        let now = scan(std::slice::from_ref(&state.pkg), &seen);
        if changed(&seen, &now).next().is_none() {
            seen = now;
            continue;
        }
        match literal_values(&state.pkg, &state.page, &now, &built) {
            Some(values) => {
                let mut s = state.live.lock().expect("the state");
                for (i, &v) in values.iter().enumerate() {
                    if s.values.get(i).is_some_and(|old| old.to_bits() != v.to_bits()) {
                        s.updates.push((i as u32, v));
                        s.values[i] = v;
                    }
                }
            }
            None => {
                let rebuilt = studio::build(&state.pkg, state.debug);
                let mut s = state.live.lock().expect("the state");
                match rebuilt {
                    Ok((out, _)) if !out.has_errors() => {
                        s.version += 1;
                        s.updates.clear();
                        s.values = build_values(&state.page);
                        s.error = None;
                        built = now.clone();
                        println!("rebuilt the lens (version {})", s.version);
                    }
                    Ok((out, _)) => {
                        let why = wrela_diag::render::render_all(&out.sources, &out.diagnostics);
                        eprint!("{why}");
                        s.error = Some(why);
                    }
                    Err(why) => {
                        eprintln!("error: {why}");
                        s.error = Some(why);
                    }
                }
            }
        }
        seen = now;
    }
}

/// The build's literals' values in the files as they are (`now`), if what changed since the
/// build (which saw the files as `built`) is literals alone ([`studio::literal_values`]).
fn literal_values(pkg: &Path, page: &Path, now: &Files, built: &Files) -> Option<Vec<f32>> {
    let r = studio::report(page).ok()?;
    // The build's report names its files from the lens.
    let lifted = studio::lifted_files(&studio::program_dir(pkg), &r, built)?;
    studio::literal_values(&lifted, &r.literals, now, built)
}

// ---- HTTP -----------------------------------------------------------------------------------------

/// The script the page gets: it asks what's new, reloads on a new build, and types each
/// literal update into the lens as `set <literal> <value>`.
const SCRIPT: &str = r#"<script>
(() => {
  let version = null, since = 0;
  const type = (line) => {
    for (const ch of line + "\n") {
      const key = ch === "\n" ? "Enter" : ch;
      const code = ch === "\n" ? "Enter" : "";
      window.dispatchEvent(new KeyboardEvent("keydown", { key, code }));
      window.dispatchEvent(new KeyboardEvent("keyup", { key, code }));
    }
  };
  const tick = async () => {
    try {
      const r = await fetch(`/studio/state?since=${since}`, { cache: "no-store" });
      const s = await r.json();
      if (version !== null && s.version !== version) { location.reload(); return; }
      version = s.version;
      for (const [i, v] of s.updates) type(`set ${i} ${v}`);
      since = s.next;
    } catch (_) {}
    setTimeout(tick, 250);
  };
  tick();
})();
</script>
"#;

fn handle(stream: TcpStream, state: &Shared) -> std::io::Result<()> {
    let Some(req) = http::read(&stream, state.port)? else { return Ok(()) };
    let mut out = stream;
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/studio/state") => {
            let since: usize = req.param("since").and_then(|v| v.parse().ok()).unwrap_or(0);
            let s = state.live.lock().expect("the state");
            let updates: Vec<serde_json::Value> =
                s.updates.iter().skip(since).map(|(i, v)| serde_json::json!([i, v])).collect();
            let answer = serde_json::json!({
                "version": s.version,
                "next": s.updates.len(),
                "updates": updates,
                "error": s.error,
            });
            http::respond(&mut out, 200, "application/json", answer.to_string().as_bytes())
        }
        ("POST", "/studio/edit") => {
            let answer = edit(state, &req.body);
            http::respond(&mut out, 200, "application/json", answer.to_string().as_bytes())
        }
        ("PUT", path) if path.starts_with("/results/") => http::put_result(
            &mut out,
            &state.page.join("results"),
            &path["/results/".len()..],
            &req.body,
        ),
        ("POST", "/studio/reference") => match &state.reference {
            Some(mask) => http::respond(&mut out, 200, "application/octet-stream", mask),
            None => {
                http::respond(&mut out, 404, "text/plain", b"no reference: serve with --reference")
            }
        },
        ("GET", path) => {
            let rel = if path == "/" { "index.html" } else { path.trim_start_matches('/') };
            http::get_file(&mut out, &state.page, rel, |html| {
                html.replace("</body>", &format!("{SCRIPT}</body>"))
            })
        }
        _ => http::respond(&mut out, 405, "text/plain", b"not allowed"),
    }
}

/// Applies the lens's edits, `{"edits": [[literal, value], ...]}`, through `wrela edit`: the
/// JSON answer (`wrela edit --json`'s), or why they were refused. The values are the lens's
/// already, so they aren't updates for it.
fn edit(state: &Shared, body: &[u8]) -> serde_json::Value {
    let answer = studio::apply(&state.pkg, &state.page, body);
    if answer["written"] == true
        && let Some(edits) = studio::parse_edits(body)
    {
        let mut s = state.live.lock().expect("the state");
        for (i, v) in edits {
            if let Some(old) = s.values.get_mut(i as usize) {
                *old = v;
            }
        }
    }
    answer
}
