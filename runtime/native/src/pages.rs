//! Pages run in Chrome for the compiler's own tests: a page directory served on loopback, opened
//! in headless Chrome stable (a hardware GPU, a throwaway profile; `wrela_chrome`), and waited
//! for until it puts `results/DONE`. The body `ok` passes; anything else is the failure's
//! message. A run empties the page's `results/` first; the page's other puts land there, and its
//! console (its workers' too, exceptions and WGSL errors among them) goes to
//! `results/console.log` as it comes.
//!
//! **The server** is this run's own, on a free port of 127.0.0.1, over the directory it's given
//! (the repository), with the toolchain's servers' rules (`crate::loopback`): a request must
//! name this machine, every answer is cross-origin isolated, a file is reached one part at a
//! time following no link, nothing under `.git` is served, and a `PUT` goes only to a plain file
//! name in a `results/` directory. [`run_url`] runs a page that another server serves (the
//! studio's) instead, and the run is done when the page's `results/DONE` appears.
//!
//! **One Chrome serves a process's runs at once**, each in a window of its own, so tests that run
//! side by side share it. It starts with the first run, and ends (its process group killed, its
//! profile removed) when the last run that holds it ends, so none outlives the runs. If it fails
//! to start, every run of the process fails the same way, and no second one is started.
//!
//! **A script** ([`wrela_chrome::script`]) gives the page trusted input while it runs, and the run
//! passes only once the script has finished too; a step that fails fails the run.

use crate::loopback;
use cap_std::fs::Dir;
use std::io::BufReader;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wrela_chrome::script::Step;
use wrela_chrome::{Browser, Launch, TabOptions};

/// How often a run looks at its page.
const LOOK: Duration = Duration::from_millis(50);

/// How long a connection's read or write may wait.
const IDLE: Duration = Duration::from_secs(10);

/// A page to run.
pub struct Page<'a> {
    /// The directory the server serves: the repository.
    pub root: &'a Path,
    /// The page's directory, from `root`, with `/` between its parts.
    pub page: &'a str,
    /// The URL's fragment: `#test&frames=...`.
    pub fragment: &'a str,
    pub timeout: Duration,
    pub script: Option<&'a [Step]>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The process's Chrome: who holds it, and why it couldn't start, if it couldn't.
#[derive(Default)]
struct Shared {
    browser: Option<Arc<Browser>>,
    users: usize,
    failed: Option<String>,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared { browser: None, users: 0, failed: None });

/// A run's hold on the process's Chrome: the last to let go ends it.
struct Lease {
    browser: Arc<Browser>,
    shared: &'static Mutex<Shared>,
}

/// Starts the process's Chrome, with a profile of its own in the process's directory of
/// profiles in the system's temporary directory (where those of processes that died are swept).
fn launch() -> Result<Browser, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let profile = wrela_chrome::profiles(&std::env::temp_dir())?.join(format!("browser-{n}"));
    Launch::new(profile).and_then(|l| Browser::launch(l, &wrela_chrome::OwnGroup))
}

impl Lease {
    fn take() -> Result<Lease, String> {
        Lease::take_from(&SHARED, launch)
    }

    /// A hold on `shared`'s Chrome, which `launch` starts if none runs: while one start is in
    /// progress, the others wait for it, and once one has failed, every later hold fails the
    /// same way, with no second start.
    fn take_from(
        shared: &'static Mutex<Shared>,
        launch: impl FnOnce() -> Result<Browser, String>,
    ) -> Result<Lease, String> {
        let held = shared;
        let mut shared = lock(held);
        if let Some(why) = &shared.failed {
            return Err(why.clone());
        }
        let browser = match &shared.browser {
            Some(b) => b.clone(),
            None => {
                let launched = launch();
                match launched {
                    Ok(b) => {
                        let b = Arc::new(b);
                        shared.browser = Some(b.clone());
                        b
                    }
                    Err(why) => {
                        let why = format!("the test browser failed to start: {why}");
                        shared.failed = Some(why.clone());
                        return Err(why);
                    }
                }
            }
        };
        shared.users += 1;
        Ok(Lease { browser, shared: held })
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let last = {
            let mut shared = lock(self.shared);
            shared.users -= 1;
            if shared.users == 0 { shared.browser.take() } else { None }
        };
        // The last hold: the browser ends when this lease's own goes, just after.
        drop(last);
    }
}

/// Empties `dir/results`: a link there is removed, not followed.
fn fresh_results(dir: &Path) -> Result<PathBuf, String> {
    let results = dir.join("results");
    match std::fs::symlink_metadata(&results) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(&results),
        Ok(_) => std::fs::remove_file(&results),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
    .map_err(|e| format!("can't clear {}: {e}", results.display()))?;
    std::fs::create_dir_all(&results)
        .map_err(|e| format!("can't make {}: {e}", results.display()))?;
    Ok(results)
}

/// Runs `page` in Chrome: `Ok` if it put `ok` in `results/DONE` (and its script finished), else
/// why not.
pub fn run(page: Page) -> Result<(), String> {
    if !page.page.split('/').all(|p| !p.is_empty() && p != "." && p != "..") {
        return Err(format!("a page's directory is a plain path from the root, not {}", page.page));
    }
    let dir = page.root.join(page.page);
    if !dir.is_dir() {
        return Err(format!("no page directory at {}", dir.display()));
    }
    let results = fresh_results(&dir)?;
    let server = Server::start(page.root, page.page)?;
    let url = format!("http://127.0.0.1:{}/{}/{}", server.port, page.page, page.fragment);
    let done = server.done.clone();
    drive(&url, &results, page.timeout, page.script, &|| lock(&done).clone())
}

/// Runs the page another server serves at `url` (the studio's) in Chrome, its results going to
/// `page/results/`: `Ok` once that's where `DONE` says `ok`, else why not.
pub fn run_url(url: &str, page: &Path, timeout: Duration) -> Result<(), String> {
    let results = fresh_results(page)?;
    let done = results.join("DONE");
    drive(url, &results, timeout, None, &|| {
        std::fs::read_to_string(&done).ok().map(|t| t.trim().to_string())
    })
}

fn drive(
    url: &str,
    results: &Path,
    timeout: Duration,
    script: Option<&[Step]>,
    done: &dyn Fn() -> Option<String>,
) -> Result<(), String> {
    let started = Instant::now();
    let deadline = started + timeout;
    let console = results.join("console.log");
    let lease = Lease::take()?;
    let options = TabOptions { size: None, console: Some(console.clone()), done: None };
    let tab = lease.browser.open(url, options, deadline)?;
    let stopped = AtomicBool::new(false);
    let stop = || stopped.load(Ordering::Acquire).then(|| "the page's run ended".to_string());
    let outcome = std::thread::scope(|sc| {
        let driving = script.map(|steps| {
            let (tab, stop) = (&tab, &stop);
            sc.spawn(move || wrela_chrome::script::run(tab, steps, deadline, stop))
        });
        let outcome = loop {
            let finished = driving.as_ref().is_none_or(|d| d.is_finished());
            let said = done();
            match said.as_deref() {
                Some("ok") if finished => break Ok(()),
                Some(why) if why != "ok" => break Err(format!("page failed: {why}")),
                _ => {}
            }
            // A script that has ended: what it gave is read once it's joined.
            if finished && driving.is_some() {
                break Ok(());
            }
            if !lease.browser.alive() {
                break Err("chrome exited early".into());
            }
            if tab.crashed() {
                break Err("the page crashed".into());
            }
            if Instant::now() >= deadline {
                break Err(format!("timed out after {:.0}s", timeout.as_secs_f64()));
            }
            std::thread::sleep(LOOK);
        };
        stopped.store(true, Ordering::Release);
        let driven =
            driving.map(|d| d.join().unwrap_or_else(|_| Err("the script panicked".into())));
        match (outcome, driven) {
            (Err(why), _) => Err(why),
            (Ok(()), Some(Err(why))) => Err(format!("the DevTools input failed: {why}")),
            // The script finished; the page must have passed too.
            (Ok(()), Some(Ok(()))) => loop {
                match done().as_deref() {
                    Some("ok") => break Ok(()),
                    Some(why) => break Err(format!("page failed: {why}")),
                    None if !lease.browser.alive() => break Err("chrome exited early".into()),
                    None if tab.crashed() => break Err("the page crashed".into()),
                    None if Instant::now() >= deadline => {
                        break Err(format!("timed out after {:.0}s", timeout.as_secs_f64()));
                    }
                    None => std::thread::sleep(LOOK),
                }
            },
            (Ok(()), None) => Ok(()),
        }
    });
    let closed = tab.close(Instant::now() + Duration::from_secs(10));
    drop(tab);
    drop(lease);
    let lines = tab_tail(&console);
    eprintln!("elapsed {:.1}s; console: {}", started.elapsed().as_secs_f64(), console.display());
    for l in &lines {
        eprintln!("  {l}");
    }
    outcome?;
    closed.map_err(|why| format!("could not close the browser test tab: {why}"))
}

/// The console's last 20 lines.
fn tab_tail(console: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(console).unwrap_or_default();
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    lines[lines.len().saturating_sub(20)..].to_vec()
}

/// The test page server: its port, what the page put in `results/DONE`, and its threads.
struct Server {
    port: u16,
    done: Arc<Mutex<Option<String>>>,
    stopping: Arc<AtomicBool>,
    conns: Conns,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The connections open, by number: what the server's end shuts.
type Conns = Arc<Mutex<std::collections::HashMap<u64, TcpStream>>>;

impl Server {
    /// Serves `root` on a free port, watching for `page/results/DONE`.
    fn start(root: &Path, page: &str) -> Result<Server, String> {
        let dir = Dir::open_ambient_dir(root, cap_std::ambient_authority())
            .map_err(|e| format!("{}: {e}", root.display()))?;
        let (listener, port) = loopback::listen(0)?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let done = Arc::new(Mutex::new(None));
        let stopping = Arc::new(AtomicBool::new(false));
        let conns = Conns::default();
        let (d, s, c, page) = (done.clone(), stopping.clone(), conns.clone(), page.to_string());
        let dir = Arc::new(dir);
        let thread = std::thread::Builder::new()
            .name("wrela-pages-server".into())
            .spawn(move || accept(&listener, &dir, port, &page, &d, &s, &c))
            .map_err(|e| format!("can't start the test server: {e}"))?;
        Ok(Server { port, done, stopping, conns, thread: Some(thread) })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        for c in lock(&self.conns).values() {
            let _ = c.shutdown(Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn accept(
    listener: &TcpListener,
    dir: &Arc<Dir>,
    port: u16,
    page: &str,
    done: &Arc<Mutex<Option<String>>>,
    stopping: &AtomicBool,
    conns: &Conns,
) {
    let mut threads: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let mut next = 0u64;
    while !stopping.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                threads.retain(|t| !t.is_finished());
                let Ok(kept) = stream.try_clone() else { continue };
                next += 1;
                let id = next;
                lock(conns).insert(id, kept);
                let (dir, done, page, open) =
                    (dir.clone(), done.clone(), page.to_string(), conns.clone());
                let spawned =
                    std::thread::Builder::new().name("wrela-pages-conn".into()).spawn(move || {
                        let _ = connection(stream, &dir, port, &page, &done);
                        // One request a connection: its end is the response's.
                        if let Some(c) = lock(&open).remove(&id) {
                            let _ = c.shutdown(Shutdown::Write);
                        }
                    });
                match spawned {
                    Ok(t) => threads.push(t),
                    Err(_) => {
                        lock(conns).remove(&id);
                    }
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    for c in lock(conns).values() {
        let _ = c.shutdown(Shutdown::Both);
    }
    for t in threads {
        let _ = t.join();
    }
}

/// Answers one request: a GET of a file, or a PUT of a page's result.
fn connection(
    stream: TcpStream,
    dir: &Dir,
    port: u16,
    page: &str,
    done: &Mutex<Option<String>>,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IDLE))?;
    stream.set_write_timeout(Some(IDLE))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    let Some(head) = loopback::read_head(&mut reader, &mut out, port)? else { return Ok(()) };
    let path = head.path.trim_start_matches('/');
    let Some(decoded) = loopback::decode_path(path) else {
        return loopback::respond(&mut out, 404, "text/plain", b"not found");
    };
    if decoded.split('/').any(|p| p.eq_ignore_ascii_case(".git")) {
        return loopback::respond(
            &mut out,
            404,
            "text/plain",
            b"the repository's history isn't served",
        );
    }
    match head.method.as_str() {
        "GET" | "HEAD" => {
            let rel = if path.is_empty() || path.ends_with('/') {
                format!("{path}index.html")
            } else {
                path.to_string()
            };
            loopback::get_file(&mut out, dir, &rel, None)
        }
        "PUT" => {
            let parts: Vec<&str> = decoded.split('/').collect();
            let refused = |out: &mut TcpStream| {
                loopback::respond(
                    out,
                    403,
                    "text/plain",
                    b"PUT is only allowed into a results/ directory",
                )
            };
            let [above @ .., "results", name] = parts.as_slice() else { return refused(&mut out) };
            if above.is_empty() || above.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
                return refused(&mut out);
            }
            let Ok(at) = crate::confined::open_dir(dir, above, false) else {
                return refused(&mut out);
            };
            let written =
                loopback::put_result(&mut out, &at, name, &mut reader, head.length as u64)?;
            if written && *name == "DONE" && above.join("/") == page {
                // One past what's read is a failure that says so, never the part that fits.
                let text = match crate::confined::read(&at, "results/DONE", 1 << 20) {
                    Ok(t) => String::from_utf8_lossy(&t).trim().to_string(),
                    Err(why) => format!("its results/DONE can't be read: {why}"),
                };
                *lock(done) = Some(text);
            }
            Ok(())
        }
        _ => loopback::respond(&mut out, 405, "text/plain", b"not allowed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn a_failed_start_fails_every_later_run_the_same_way_with_no_second_start() {
        static SHARED: Mutex<Shared> = Mutex::new(Shared { browser: None, users: 0, failed: None });
        let starts = std::sync::atomic::AtomicU32::new(0);
        let failing = || {
            starts.fetch_add(1, Ordering::Relaxed);
            Err("it exited with status 1".to_string())
        };
        let first = Lease::take_from(&SHARED, failing).err().expect("a failed start");
        let later = Lease::take_from(&SHARED, || panic!("no second start")).err().expect("failed");
        assert_eq!(first, "the test browser failed to start: it exited with status 1");
        assert_eq!(later, first);
        assert_eq!(starts.load(Ordering::Relaxed), 1);
        assert_eq!(lock(&SHARED).users, 0, "nobody holds a browser that never started");
    }

    fn exchange(port: u16, request: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        s.write_all(request.as_bytes()).expect("send");
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    #[test]
    fn the_test_server_serves_files_takes_results_and_hears_done() {
        let root = std::env::temp_dir().join(format!("wrela-pages-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("p/results")).expect("dirs");
        std::fs::create_dir_all(root.join(".git")).expect("dirs");
        std::fs::write(root.join("p/index.html"), "<p>hi</p>").expect("page");
        std::fs::write(root.join(".git/config"), "secret").expect("git");
        let server = Server::start(&root, "p").expect("serve");
        let port = server.port;
        let get = |path: &str| {
            exchange(port, &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"))
        };
        let page = get("/p/");
        assert!(page.starts_with("HTTP/1.1 200") && page.ends_with("<p>hi</p>"), "{page}");
        assert!(page.contains("Cross-Origin-Embedder-Policy: require-corp"));
        assert!(get("/.git/config").starts_with("HTTP/1.1 404"));
        let put = |path: &str, body: &str| {
            exchange(
                port,
                &format!(
                    "PUT {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                ),
            )
        };
        assert!(put("/p/notes.txt", "x").starts_with("HTTP/1.1 403"), "only into results/");
        assert!(put("/p/results/frame.json", "{}").starts_with("HTTP/1.1 201"));
        assert_eq!(std::fs::read_to_string(root.join("p/results/frame.json")).expect("put"), "{}");
        assert!(lock(&server.done).is_none());
        assert!(put("/p/results/DONE", "ok\n").starts_with("HTTP/1.1 201"));
        assert_eq!(lock(&server.done).as_deref(), Some("ok"));
        drop(server);
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err(), "its port is closed");
        let _ = std::fs::remove_dir_all(&root);
    }
}
