//! `std::tool::browser` (language.md §15.5): Chrome stable, headless, on the hardware GPU, with a
//! throwaway profile in the host's own directory for the run, driven over the DevTools protocol
//! on Chrome's pipe (`wrela_chrome`). `open` starts a browser of its own for a page; `run`
//! carries out a script's typed steps; the observations read what the page shows; `capture`
//! gives its screen as a PNG; `close` ends it. Starting one needs the `--browser` grant.
//!
//! **A browser reaches only what the tool is granted.** Every connection it makes, of its
//! pages, their workers and Chrome itself, goes through a proxy of the host's
//! (`wrela_chrome::proxy`), with no implicit bypass, and QUIC and WebTransport off. A plain HTTP
//! request goes on only to an origin of one of the tool's running servers (`std::tool::serve`,
//! `http://127.0.0.1:<port>`); a tunnel (HTTPS, a WebSocket) only to a host and port that
//! `--net` grants (443 for a grant with none). Anything else is refused before a connection is
//! made, and the page's console says so. A tool test's fake sites aren't the browser's.
//!
//! **A page's result is what its server kept, for this browser.** `results` and a wait for it
//! read the `results/DONE` that the page's server (the tool's own, on the port of the URL it
//! opened) has written from a request of this browser's: the proxy names each request with the
//! browser's session, and the server keeps a result for each session. A request in flight, one
//! the server refused or hasn't finished, and another browser's page's, aren't this one's.
//!
//! **A browser is a resource of its request.** Its process group is recorded for the
//! supervisor (`run::let_go`) before Chrome runs; it's registered under the `open` request as
//! soon as Chrome answers, before its page opens and before the answer that names it reaches
//! the tool. If the request fails, or the tool ends first, the host closes it: the group killed,
//! the profile removed. A start in progress when the tool ends is given up. Chrome holds the
//! host's end of its pipe, so if the host is killed it shuts down too, and the run's directory
//! is swept at the next run.
//!
//! **Closing is never held up.** Each browser has a cancel flag, which `close` sets as the host
//! takes the request (before any worker does: every worker may be busy, with this browser's
//! wait among them), or the tool's end sets, then closes the browser's pipe: every request using it
//! (a wait, even of 600 s, reads the flag as it waits) fails at once, and lets the page go; then
//! `close` takes the page within the grace, or the run is the host's failure.
//!
//! **Deadlines count from the request.** Each request has one (60 s, or its `timeout`, at most
//! 600 s), from when the host took it; a script's waits and clicks are bounded by it too. A
//! capture reserves its blob's most bytes (a PNG of the viewport, stored) in the budget before
//! it asks Chrome for the screen. No observation is cut short: one whose answer would be more
//! than 512 KiB fails, saying so.

use super::caps::MOST_BYTES;
use super::services::{Body, Resources, Services, Work, err, ok};
use super::toolchain::Out;
use crate::locked;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use wrela_chrome::script::{Element, Query, Seen};
use wrela_chrome::{Browser, Dest, Launch, Spawner, TabOptions};

/// A request's deadline unless it asks for another.
const DEADLINE: Duration = Duration::from_secs(60);

/// The browsers of one run ([`Resources`]).
pub(crate) type Browsers = Resources<Held>;

impl Browsers {
    /// Cancels every browser's requests and closes its pipe: each request waiting on Chrome, or
    /// on what it's seen, fails at once.
    pub(crate) fn disconnect_all(&self) {
        for b in self.all() {
            b.cancel();
        }
    }
}

/// A browser and its page, whether a request is using it, and its cancel flag.
pub(crate) struct Held {
    browser: Browser,
    tab: Mutex<Option<wrela_chrome::Tab>>,
    busy: AtomicBool,
    cancelled: AtomicBool,
    /// How long `close` may wait for a request to let the page go.
    grace: Duration,
}

impl Held {
    /// Sets the cancel flag, then closes the pipe: what uses the browser fails at once.
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.browser.disconnect();
    }

    /// [`Held::cancel`], waiting for nothing: as the host takes a close, before any worker does.
    fn cut(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.browser.cut();
    }

    /// Closes it: cancels what uses it, takes its page within the grace, then ends its process
    /// group, profile and proxy. Whether that ended cleanly.
    pub(crate) fn close(&self) -> bool {
        self.cancel();
        let until = Instant::now() + self.grace;
        let tab = loop {
            match self.tab.try_lock() {
                Ok(mut t) => break Some(t.take()),
                Err(std::sync::TryLockError::Poisoned(p)) => break Some(p.into_inner().take()),
                Err(std::sync::TryLockError::WouldBlock) if Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                // A request that didn't let the page go: the browser ends under it anyway.
                Err(std::sync::TryLockError::WouldBlock) => break None,
            }
        };
        let took = tab.is_some();
        if let Some(Some(t)) = tab {
            let _ = t.close(Instant::now() + Duration::from_millis(100));
        }
        let ended = self.browser.close();
        took && ended
    }
}

/// A request's deadline: the `f64` of seconds at its end (0: [`DEADLINE`]).
fn deadline(b: &mut Body) -> Result<Duration, String> {
    let d = super::http::deadline(f64::from_bits(b.u64()?), DEADLINE)?;
    b.end()?;
    Ok(d)
}

fn element(b: &mut Body) -> Result<Element, String> {
    let css = b.str()?;
    if css.is_empty() {
        return Err("an element's selector is empty".into());
    }
    let text = match b.u8()? {
        0 => None,
        1 => Some(b.str()?),
        k => return Err(format!("{k} isn't whether an element has a text")),
    };
    Ok(Element { css, text })
}

/// What an observation reads: a page's query, its console, or its result.
enum Look {
    Page(Query),
    Console,
    Results,
}

/// How request `name` of the browser service, with `b`, is carried out. A close cancels its
/// browser here, as the host takes it: a request using the browser fails at once, and frees its
/// worker, though every worker is busy and the close waits for one.
pub(crate) fn route(s: &Services, name: &str, b: &mut Body) -> Work {
    let entered = Instant::now();
    let parsed = (|| -> Result<Work, String> {
        Ok(match name {
            "open" => {
                let url = b.str()?;
                let (w, h) = (b.u32()?, b.u32()?);
                if !(1..=16384).contains(&w) || !(1..=16384).contains(&h) {
                    return Err(format!("a viewport of {w}×{h} isn't 1 to 16384 each way"));
                }
                let due = entered + deadline(b)?;
                Work::Long(Box::new(move |s, request| {
                    super::services::answer(open(s, request, &url, (w, h), due))
                }))
            }
            "run" => {
                let handle = b.u32()?;
                let steps = wrela_chrome::script::parse(&b.str()?)?;
                let limit = deadline(b)?;
                let due = entered + limit;
                Work::Long(Box::new(move |s, _| {
                    super::services::answer(using(s, handle, due, |held, tab| {
                        let closing = s.closing.clone();
                        let stop = move || {
                            if held.cancelled.load(Ordering::Acquire) {
                                return Some("the browser was closed".to_string());
                            }
                            closing.load(Ordering::Acquire).then(|| "the tool ended".to_string())
                        };
                        s.signal("run");
                        let ran = wrela_chrome::script::run(tab, &steps, due, &stop);
                        ran.map(|()| Vec::new()).map_err(|why| {
                            if Instant::now() >= due {
                                let s = limit.as_secs_f64();
                                format!("`run` ran past its deadline of {s} s: {why}")
                            } else {
                                why
                            }
                        })
                    }))
                }))
            }
            "observe" => {
                let handle = b.u32()?;
                let kind = b.u8()?;
                let timeout = f64::from_bits(b.u64()?);
                let look = match kind {
                    0 => Look::Page(Query::Title),
                    1 => Look::Page(Query::Location),
                    2 => Look::Page(Query::Clipboard),
                    3 => Look::Page(Query::Count(element(b)?)),
                    4 => Look::Page(Query::Texts(b.str()?)),
                    5 => {
                        let e = element(b)?;
                        Look::Page(Query::Attribute(e, b.str()?))
                    }
                    6 => Look::Page(Query::Active),
                    7 => Look::Console,
                    8 => Look::Results,
                    k => return Err(format!("{k} isn't an observation")),
                };
                b.end()?;
                let due = entered + super::http::deadline(timeout, DEADLINE)?;
                Work::Long(Box::new(move |s, _| {
                    super::services::answer(using(s, handle, due, |_, tab| {
                        observe(tab, &look, due)
                    }))
                }))
            }
            "capture" => {
                let handle = b.u32()?;
                let due = entered + deadline(b)?;
                Work::Long(Box::new(move |s, request| {
                    super::services::answer(using(s, handle, due, |_, tab| {
                        capture(s, request, tab, due)
                    }))
                }))
            }
            "close" => {
                let handle = b.u32()?;
                b.end()?;
                let held = s.browsers().find(handle, "browser");
                if let Ok(held) = held {
                    held.cut();
                }
                s.signal("closing");
                Work::Long(Box::new(move |s, _| {
                    s.signal("close");
                    let held = s.browsers().take(handle, "browser");
                    let held = match held {
                        Ok(held) => held,
                        Err(why) => return err(&why),
                    };
                    if !held.close() {
                        let why = format!("the browser {handle} didn't end cleanly");
                        let _ = s.stuck.set(why.clone());
                        return err(&why);
                    }
                    ok(&[])
                }))
            }
            _ => return Err(format!("the browser service has no `{name}`")),
        })
    })();
    parsed.unwrap_or_else(|why| Work::Now(err(&why)))
}

/// Whether a connection of the tool's browser may go to `dest`: plain HTTP only to one of its
/// running servers, `http://127.0.0.1:<port>`; a tunnel (HTTPS, a WebSocket) only to a host and
/// port `--net` grants (443 for a grant with none), not a tool test's fake.
fn allowed(s: &Services, dest: Dest) -> bool {
    match dest {
        Dest::Http { host, port } => host == "127.0.0.1" && s.servers().ports().contains(&port),
        Dest::Tunnel { host, port } => {
            let named = format!("{host}:{port}");
            s.caps.host(&named) == Some(false) || (port == 443 && s.caps.host(host) == Some(false))
        }
    }
}

/// The port of `url`'s origin, if it's `http://127.0.0.1:<port>`.
fn served_port(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("http://127.0.0.1:")?;
    rest.split(['/', '?', '#']).next()?.parse().ok()
}

/// Whether the tool may open `url`: an origin of one of its servers, or a granted `https://`.
fn may_open(s: &Services, url: &str) -> bool {
    if let Some(port) = served_port(url) {
        return allowed(s, Dest::Http { host: "127.0.0.1", port });
    }
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().unwrap_or(0)),
        None => (authority, 443),
    };
    allowed(s, Dest::Tunnel { host: &host.to_ascii_lowercase(), port })
}

/// The tool host's way to start Chrome: through `run::owned_command`, so a supervisor's file of
/// groups has Chrome's before it runs.
struct Owned<'a> {
    s: &'a Services,
    go: std::path::PathBuf,
}

impl Spawner for Owned<'_> {
    fn command(
        &self,
        exe: &std::path::Path,
        args: &[std::ffi::OsString],
    ) -> Result<std::process::Command, String> {
        super::run::owned_command(self.s, exe, args.to_vec(), &self.go)
    }

    fn started(&self, child: &mut std::process::Child) -> Result<i32, String> {
        super::run::let_go(self.s, child, &self.go)
    }
}

/// Opens `url` in a browser of its own for `request`, by `due`: its handle.
fn open(
    s: &Arc<Services>,
    request: u32,
    url: &str,
    size: (u32, u32),
    due: Instant,
) -> Result<Vec<u8>, String> {
    if !s.caps.browser() {
        return Err("the browser isn't granted: `--browser` grants it".into());
    }
    if !may_open(s, url) {
        return Err(format!(
            "`{url}` isn't one of the tool's servers' origins or an `https://` host it's granted"
        ));
    }
    let left = due.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err("`open` didn't start within its deadline".into());
    }
    let handle = s.browsers().reserve();
    let profile = s.work_dir()?.join(format!("browser-{handle}"));
    let mut launch = Launch::new(profile)?;
    launch.window = size;
    launch.startup = launch.startup.min(left);
    launch.cancel = Some(s.closing.clone());
    // What the proxy names this browser's requests with: its results are its own.
    let session = format!("browser-{handle}");
    launch.session = Some(session.clone());
    let weak: Weak<Services> = Arc::downgrade(s);
    launch.policy = Some(Arc::new(move |d: Dest| weak.upgrade().is_some_and(|s| allowed(&s, d))));
    let spawner = Owned { s, go: s.work_dir()?.join(format!("{request}.browser.go")) };
    let launched = Browser::launch(launch, &spawner);
    // Chrome has started (or won't): the file that let it go has done its work.
    let _ = std::fs::remove_file(&spawner.go);
    let browser = launched?;
    let held = Arc::new(Held {
        browser,
        tab: Mutex::new(None),
        busy: AtomicBool::new(true),
        cancelled: AtomicBool::new(false),
        grace: s.grace,
    });
    // Registered under its request before its page opens, and before any answer names it.
    s.browsers().hold(request, handle, held.clone());
    if s.closing.load(Ordering::Acquire) {
        // The tool ended as it started: its end may have closed every pipe already.
        return Err("the tool ended before the browser opened".into());
    }
    // The page's result: what its server keeps from now on.
    let done = served_port(url).and_then(|port| s.servers().on_port(port)).map(|server| {
        let session = session.clone();
        let d: wrela_chrome::Done = Arc::new(move || server.done(&session));
        d
    });
    let options = TabOptions { size: Some(size), console: None, done };
    let tab = held.browser.open(url, options, due)?;
    // A page between documents has no context to ask yet: until it has one.
    loop {
        match tab.hardware_adapter(due) {
            Ok(()) => break,
            Err(why) if why.contains("xecution context") && Instant::now() < due => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(why) => return Err(why),
        }
    }
    *locked(&held.tab) = Some(tab);
    held.busy.store(false, Ordering::Release);
    s.signal("browser");
    let mut out = Out::default();
    out.u32(handle);
    Ok(out.0)
}

/// Does `work` with `handle`'s browser and page, by `due`, one request at a time.
fn using(
    s: &Services,
    handle: u32,
    due: Instant,
    work: impl FnOnce(&Held, &wrela_chrome::Tab) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    if Instant::now() >= due {
        return Err("the request didn't start within its deadline".into());
    }
    let held = s.browsers().find(handle, "browser")?;
    if held.cancelled.load(Ordering::Acquire) {
        return Err(format!("the browser {handle} was closed"));
    }
    if held.busy.swap(true, Ordering::AcqRel) {
        return Err(format!("the browser {handle} is busy with another request"));
    }
    struct Idle<'a>(&'a AtomicBool);
    impl Drop for Idle<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let _idle = Idle(&held.busy);
    let tab = locked(&held.tab);
    let Some(tab) = tab.as_ref() else { return Err(format!("the browser {handle} has no page")) };
    work(&held, tab)
}

/// An observation's answer, whole: one past [`MOST_BYTES`] fails, rather than be cut short.
fn whole(out: Out, what: &str) -> Result<Vec<u8>, String> {
    if out.0.len() > MOST_BYTES {
        return Err(format!(
            "{what} is {} bytes, more than an answer holds ({MOST_BYTES}): observe less of the page",
            out.0.len()
        ));
    }
    Ok(out.0)
}

/// What `look` reads of `tab`'s page, as `std::tool::browser`'s readers read it, whole.
fn observe(tab: &wrela_chrome::Tab, look: &Look, due: Instant) -> Result<Vec<u8>, String> {
    let mut out = Out::default();
    match look {
        Look::Console => {
            out.strs(&tab.console()?);
            return whole(out, "the console");
        }
        Look::Results => {
            if !tab.knows_done() {
                return Err(
                    "the page isn't served by one of the tool's servers, which keep its results"
                        .into(),
                );
            }
            match tab.done() {
                Some(d) => out.u8(1).str(&d?),
                None => out.u8(0),
            };
        }
        Look::Page(q) => match wrela_chrome::script::observe(tab, q, due)? {
            Seen::Text(t) => {
                out.str(&t);
            }
            Seen::Texts(t) => {
                out.strs(&t);
            }
            Seen::Count(n) => {
                out.u32(n);
            }
            Seen::Attribute(a) => match a {
                Some(a) => {
                    out.u8(1).str(&a);
                }
                None => {
                    out.u8(0);
                }
            },
            Seen::Active(a) => match a {
                Some((tag, id)) => {
                    out.u8(1).str(&tag).str(&id);
                }
                None => {
                    out.u8(0);
                }
            },
        },
    }
    whole(out, "the observation")
}

/// The most bytes a PNG of `w` × `h` RGBA pixels takes: its rows stored, each with its filter
/// byte, deflate's block headers, and its chunks.
fn png_bound(w: u32, h: u32) -> u64 {
    let raw = u64::from(h) * (1 + 4 * u64::from(w));
    raw + raw / 65535 * 5 + 64 * 1024
}

/// `tab`'s screen for `request`: the blob and its bytes reserved first, then filled.
fn capture(
    s: &Services,
    request: u32,
    tab: &wrela_chrome::Tab,
    due: Instant,
) -> Result<Vec<u8>, String> {
    let (w, h) = tab.size();
    let most = png_bound(w, h);
    let id = s
        .blobs()
        .create_reserved(request, most)
        .map_err(|why| format!("the screen's {most} bytes: {why}"))?;
    // The blob is the request's: a failure gives it back.
    let png = tab.screenshot(most as usize, due)?;
    s.blobs().fill(id, png)?;
    let mut out = Out::default();
    out.u32(w).u32(h).u8(1).u32(id);
    Ok(out.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pngs_bound_holds_its_rows_stored() {
        assert!(png_bound(1, 1) >= 5);
        let raw = 1080 * (1 + 4 * 1920);
        assert!(png_bound(1920, 1080) > raw && png_bound(1920, 1080) < raw + raw / 100 + 65536);
    }

    #[test]
    fn a_served_url_names_its_port() {
        assert_eq!(served_port("http://127.0.0.1:8123/page.html#test"), Some(8123));
        assert_eq!(served_port("http://127.0.0.1:8123"), Some(8123));
        assert_eq!(served_port("http://localhost:8123/"), None);
        assert_eq!(served_port("https://127.0.0.1:8123/"), None);
    }
}
