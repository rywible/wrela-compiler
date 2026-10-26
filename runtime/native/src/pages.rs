//! Pages run in Chrome for the compiler's own tests: a page directory served on loopback, opened
//! in headless Chrome stable (a hardware GPU, a throwaway profile; `wrela_chrome`), and waited
//! for until it puts `results/DONE`. The body `ok` passes; anything else is the failure's
//! message. A run empties the page's `results/` first; the page's other puts land there, and its
//! console (its workers' too, exceptions and WGSL errors among them) goes to
//! `results/console.log` as it comes.
//!
//! **The server** is this run's own, on a free port of 127.0.0.1, over the directory it's given
//! (the repository, or a build's directory that is the page itself, so that nothing beside it is
//! served), with the toolchain's servers' rules (`crate::loopback`): a request must
//! name this machine, every answer is cross-origin isolated, a file is reached one part at a
//! time following no link, nothing under `.git` is served, and a `PUT` goes only to a plain file
//! name in a `results/` directory. [`run_url`] runs a page that another server serves (`wrela
//! run`'s) instead, and the run is done when the page's `results/DONE` appears. The server keeps at
//! most [`MOST_CONNECTIONS`] requests open, each for at most [`REQUEST_DEADLINE`] in total.
//!
//! **One Chrome serves a process's runs at once**, each in a window of its own, so tests that run
//! side by side share it. It starts with the first run, and ends (its process group killed, its
//! profile removed) when the last run that holds it ends, so none outlives the runs. If it fails
//! to start, every run of the process fails the same way, and no second one is started.
//!
//! **A script** ([`wrela_chrome::script`]) gives the page trusted input while it runs, and the run
//! passes only once the script has finished too; a step that fails fails the run.

use crate::loopback::DeadlineIo;
use crate::{confined, locked, loopback};
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

/// The most time one request may occupy a connection, even if its peer keeps sending bytes.
const REQUEST_DEADLINE: Duration = Duration::from_secs(30);

/// The most request threads this test server starts at once.
const MOST_CONNECTIONS: usize = 64;

/// A page to run.
pub struct Page<'a> {
    /// The directory the server serves: the repository, or the page's own directory.
    pub root: &'a Path,
    /// The page's directory, from `root`, with `/` between its parts; empty: `root` itself is
    /// the page, at `/`, so nothing beside it is served.
    pub page: &'a str,
    /// The URL's fragment: `#test&frames=...`.
    pub fragment: &'a str,
    pub timeout: Duration,
    pub script: Option<&'a [Step]>,
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
        let mut shared = locked(held);
        if let Some(why) = &shared.failed {
            return Err(why.clone());
        }
        let browser = match &shared.browser {
            Some(b) => b.clone(),
            None => match launch() {
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
            },
        };
        shared.users += 1;
        Ok(Lease { browser, shared: held })
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let last = {
            let mut shared = locked(self.shared);
            shared.users -= 1;
            if shared.users == 0 { shared.browser.take() } else { None }
        };
        // The last hold: the browser ends when this lease's own goes, just after.
        drop(last);
    }
}

/// Empties `dir/results` through `opened`: a link there is removed, not followed, and a
/// directory that was opened as the page stays the page if its path later changes.
fn fresh_results(dir: &Path, opened: &Dir) -> Result<PathBuf, String> {
    let results = dir.join("results");
    match opened.symlink_metadata("results") {
        Ok(m) if m.is_dir() => opened.remove_dir_all("results"),
        Ok(_) => opened.remove_file("results"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
    .map_err(|e| format!("can't clear {}: {e}", results.display()))?;
    opened.create_dir("results").map_err(|e| format!("can't make {}: {e}", results.display()))?;
    Ok(results)
}

/// Runs `page` in Chrome: `Ok` if it put `ok` in `results/DONE` (and its script finished), else
/// why not.
pub fn run(page: Page) -> Result<(), String> {
    run_opens(page, 1)
}

/// Runs `page` in Chrome twice, as a player opens a game a second time: the same browser
/// profile, the same server and port, so the second open finds what the first left in the
/// browser's caches. The first open's results are kept in `results-first/`; the second's are in
/// `results/`.
pub fn run_twice(page: Page) -> Result<(), String> {
    run_opens(page, 2)
}

fn run_opens(page: Page, opens: usize) -> Result<(), String> {
    let root = Dir::open_ambient_dir(page.root, cap_std::ambient_authority())
        .map_err(|e| format!("{}: {e}", page.root.display()))?;
    let (dir, opened, path) = if page.page.is_empty() {
        let opened = root.try_clone().map_err(|e| format!("{}: {e}", page.root.display()))?;
        (page.root.to_path_buf(), opened, String::new())
    } else {
        if !page.page.split('/').all(|p| !p.is_empty() && p != "." && p != "..") {
            return Err(format!(
                "a page's directory is a plain path from the root, not {}",
                page.page
            ));
        }
        let dir = page.root.join(page.page);
        let parts = confined::parts(page.page)?;
        let opened = confined::open_dir(&root, &parts, false)
            .map_err(|e| format!("no plain page directory at {}: {e}", dir.display()))?;
        (dir, opened, format!("{}/", page.page))
    };
    let mut results = fresh_results(&dir, &opened)?;
    let server = Server::start(root, page.page)?;
    let url = format!("http://127.0.0.1:{}/{path}{}", server.port, page.fragment);
    let done = server.done.clone();
    // Held across the opens: the browser, and its profile, last until the last one ends.
    let _browser = Lease::take()?;
    for open in 0..opens {
        if open > 0 {
            let first = dir.join("results-first");
            let _ = std::fs::remove_dir_all(&first);
            std::fs::rename(&results, &first)
                .map_err(|e| format!("can't keep the first open's results: {e}"))?;
            results = fresh_results(&dir, &opened)?;
            *locked(&done) = None;
        }
        drive(&url, &results, page.timeout, page.script, &|| locked(&done).clone())?;
    }
    Ok(())
}

/// Runs the page another server serves at `url` (`wrela run`'s) in Chrome, its results going to
/// `page/results/`: `Ok` once that's where `DONE` says `ok`, else why not.
pub fn run_url(url: &str, page: &Path, timeout: Duration) -> Result<(), String> {
    let absolute = std::path::absolute(page).map_err(|e| format!("{}: {e}", page.display()))?;
    let parent = absolute.parent().ok_or_else(|| format!("{} has no parent", page.display()))?;
    let name = absolute
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} has no plain directory name", page.display()))?;
    confined::parts(name)?;
    let above = Dir::open_ambient_dir(parent, cap_std::ambient_authority())
        .map_err(|e| format!("{}: {e}", parent.display()))?;
    let opened = confined::open_dir(&above, &[name], true)
        .map_err(|e| format!("{}: {e}", page.display()))?;
    let results = fresh_results(page, &opened)?;
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
    // Why the page can't pass any more, if it can't.
    let trouble = || {
        if !lease.browser.alive() {
            Some("chrome exited early".to_string())
        } else if tab.crashed() {
            Some("the page crashed".to_string())
        } else if Instant::now() >= deadline {
            Some(format!("timed out after {:.0}s", timeout.as_secs_f64()))
        } else {
            None
        }
    };
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
            if let Some(why) = trouble() {
                break Err(why);
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
                    None => match trouble() {
                        Some(why) => break Err(why),
                        None => std::thread::sleep(LOOK),
                    },
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
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(20)..].iter().map(|l| l.to_string()).collect()
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

/// What every connection's thread shares.
struct ServerState {
    dir: Dir,
    port: u16,
    page: String,
    done: Arc<Mutex<Option<String>>>,
    stopping: Arc<AtomicBool>,
    conns: Conns,
    most_connections: usize,
    request_deadline: Duration,
}

impl Server {
    /// Serves the root `dir` on a free port, watching for `page/results/DONE` (`results/DONE`
    /// with an empty `page`: the root is the page).
    fn start(dir: Dir, page: &str) -> Result<Server, String> {
        Self::start_with(dir, page, MOST_CONNECTIONS, REQUEST_DEADLINE)
    }

    fn start_with(
        dir: Dir,
        page: &str,
        most_connections: usize,
        request_deadline: Duration,
    ) -> Result<Server, String> {
        let (listener, port) = loopback::listen(0)?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let done = Arc::new(Mutex::new(None));
        let stopping = Arc::new(AtomicBool::new(false));
        let conns = Conns::default();
        let state = Arc::new(ServerState {
            dir,
            port,
            page: page.to_string(),
            done: done.clone(),
            stopping: stopping.clone(),
            conns: conns.clone(),
            most_connections,
            request_deadline,
        });
        let thread = std::thread::Builder::new()
            .name("wrela-pages-server".into())
            .spawn(move || accept(&listener, state))
            .map_err(|e| format!("can't start the test server: {e}"))?;
        Ok(Server { port, done, stopping, conns, thread: Some(thread) })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        for c in locked(&self.conns).values() {
            let _ = c.shutdown(Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn accept(listener: &TcpListener, state: Arc<ServerState>) {
    let mut threads: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let mut next = 0u64;
    while !state.stopping.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                threads.retain(|t| !t.is_finished());
                let mut open = locked(&state.conns);
                if state.stopping.load(Ordering::Acquire) || open.len() >= state.most_connections {
                    continue;
                }
                let Ok(kept) = stream.try_clone() else { continue };
                next += 1;
                let id = next;
                open.insert(id, kept);
                drop(open);
                let due = Instant::now() + state.request_deadline;
                let shared = state.clone();
                let spawned =
                    std::thread::Builder::new().name("wrela-pages-conn".into()).spawn(move || {
                        let s = &shared;
                        let _ = connection(stream, &s.dir, s.port, &s.page, &s.done, due);
                        // One request a connection: its end is the response's.
                        if let Some(c) = locked(&s.conns).remove(&id) {
                            let _ = c.shutdown(Shutdown::Write);
                        }
                    });
                match spawned {
                    Ok(t) => threads.push(t),
                    Err(_) => {
                        locked(&state.conns).remove(&id);
                    }
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    for c in locked(&state.conns).values() {
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
    due: Instant,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    let mut reader = BufReader::new(DeadlineIo { stream: stream.try_clone()?, due, idle: IDLE });
    let mut out = DeadlineIo { stream, due, idle: IDLE };
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
            loopback::get_file(
                &mut out,
                dir,
                &rel,
                None,
                loopback::Caching::Revalidate { if_none_match: head.if_none_match.as_deref() },
            )
        }
        "PUT" => {
            let parts: Vec<&str> = decoded.split('/').collect();
            let refused = |out: &mut DeadlineIo| {
                loopback::respond(
                    out,
                    403,
                    "text/plain",
                    b"PUT is only allowed into a results/ directory",
                )
            };
            let [above @ .., "results", name] = parts.as_slice() else { return refused(&mut out) };
            // The root's own `results/` only when the root is the page.
            if (above.is_empty() && !page.is_empty())
                || above.iter().any(|p| p.is_empty() || *p == "." || *p == "..")
            {
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
                *locked(done) = Some(text);
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

    fn root_dir(root: &Path) -> Dir {
        Dir::open_ambient_dir(root, cap_std::ambient_authority()).expect("the root")
    }

    fn wait_for_connections(server: &Server, count: usize) {
        let until = Instant::now() + Duration::from_secs(2);
        while locked(&server.conns).len() != count && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(locked(&server.conns).len(), count);
    }

    #[test]
    fn the_test_server_bounds_open_connections() {
        let root = std::env::temp_dir().join(format!("wrela-pages-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("p")).expect("page");
        let server =
            Server::start_with(root_dir(&root), "p", 2, Duration::from_secs(2)).expect("server");
        let first = TcpStream::connect(("127.0.0.1", server.port)).expect("first");
        let second = TcpStream::connect(("127.0.0.1", server.port)).expect("second");
        wait_for_connections(&server, 2);
        let mut excess = TcpStream::connect(("127.0.0.1", server.port)).expect("excess");
        excess.set_read_timeout(Some(Duration::from_secs(1))).expect("read timeout");
        let mut byte = [0];
        let result = excess.read(&mut byte);
        let closed = match &result {
            Ok(0) => true,
            Err(e) => {
                !matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock)
            }
            _ => false,
        };
        assert!(closed, "excess connection is closed: {result:?}");
        assert_eq!(locked(&server.conns).len(), 2, "no request thread for the excess connection");
        drop(server);
        drop((first, second));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_trickling_request_cannot_hold_a_test_server_thread_past_its_deadline() {
        let root =
            std::env::temp_dir().join(format!("wrela-pages-deadline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("p")).expect("page");
        let server = Server::start_with(root_dir(&root), "p", 2, Duration::from_millis(100))
            .expect("server");
        let mut slow = TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
        wait_for_connections(&server, 1);
        for _ in 0..12 {
            if slow.write_all(b"G").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        wait_for_connections(&server, 0);
        drop(server);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_page_cannot_clear_results_outside_the_root() {
        let base =
            std::env::temp_dir().join(format!("wrela-pages-linked-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::create_dir_all(outside.join("results")).expect("outside results");
        std::fs::write(outside.join("results/keep"), "keep").expect("outside file");
        std::os::unix::fs::symlink(&outside, root.join("page")).expect("linked page");
        let result = run(Page {
            root: &root,
            page: "page",
            fragment: "",
            timeout: Duration::from_secs(1),
            script: None,
        });
        assert!(result.is_err(), "a linked page is not served");
        assert_eq!(std::fs::read(outside.join("results/keep")).expect("kept"), b"keep");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_run_url_page_cannot_clear_results_outside_its_path() {
        let base =
            std::env::temp_dir().join(format!("wrela-pages-linked-url-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("base");
        let outside = base.join("outside");
        std::fs::create_dir_all(outside.join("results")).expect("outside results");
        std::fs::write(outside.join("results/keep"), "keep").expect("outside file");
        let page = base.join("page");
        std::os::unix::fs::symlink(&outside, &page).expect("linked page");
        let result = run_url("http://127.0.0.1:1/", &page, Duration::from_secs(1));
        assert!(result.is_err(), "a linked page is not run");
        assert_eq!(std::fs::read(outside.join("results/keep")).expect("kept"), b"keep");
        let _ = std::fs::remove_dir_all(&base);
    }

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
        assert_eq!(locked(&SHARED).users, 0, "nobody holds a browser that never started");
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
        let server = Server::start(root_dir(&root), "p").expect("serve");
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
        assert!(put("/results/x", "x").starts_with("HTTP/1.1 403"), "the root isn't the page");
        assert!(put("/p/results/frame.json", "{}").starts_with("HTTP/1.1 201"));
        assert_eq!(std::fs::read_to_string(root.join("p/results/frame.json")).expect("put"), "{}");
        assert!(locked(&server.done).is_none());
        assert!(put("/p/results/DONE", "ok\n").starts_with("HTTP/1.1 201"));
        assert_eq!(locked(&server.done).as_deref(), Some("ok"));
        drop(server);
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err(), "its port is closed");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A root that is the page (an empty `page`, as a build's checks run it) is served at `/`,
    /// takes its results and hears their `DONE`, and serves nothing beside it.
    #[test]
    fn a_root_that_is_the_page_serves_nothing_beside_it() {
        let base = std::env::temp_dir().join(format!("wrela-pages-own-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let build = base.join("build");
        std::fs::create_dir_all(build.join("results")).expect("dirs");
        std::fs::write(build.join("index.html"), "<p>hi</p>").expect("page");
        std::fs::write(base.join("beside.txt"), "secret").expect("beside");
        std::fs::create_dir_all(base.join("other/results")).expect("other");
        let server = Server::start(root_dir(&build), "").expect("serve");
        let port = server.port;
        let ask = |method: &str, path: &str, body: &str| {
            exchange(
                port,
                &format!(
                    "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                ),
            )
        };
        let page = ask("GET", "/", "");
        assert!(page.starts_with("HTTP/1.1 200") && page.ends_with("<p>hi</p>"), "{page}");
        for path in ["/../beside.txt", "/%2e%2e/beside.txt", "/..%2fbeside.txt"] {
            let got = ask("GET", path, "");
            assert!(!got.starts_with("HTTP/1.1 2") && !got.contains("secret"), "{path}: {got}");
        }
        for path in ["/../other/results/x", "/%2e%2e/other/results/x"] {
            assert!(!ask("PUT", path, "x").starts_with("HTTP/1.1 2"), "{path}");
        }
        assert!(!base.join("other/results/x").exists(), "nothing is put beside the page");
        assert!(ask("PUT", "/results/DONE", "ok\n").starts_with("HTTP/1.1 201"));
        assert_eq!(locked(&server.done).as_deref(), Some("ok"));
        drop(server);
        let _ = std::fs::remove_dir_all(&base);
    }
}
