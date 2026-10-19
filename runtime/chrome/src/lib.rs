//! Chrome stable, driven over the DevTools protocol (CDP) from Rust: for the tool host's
//! browser service (`std::tool::browser`, language.md §15.5) and the compiler's own browser
//! tests. Chrome runs headless (`--headless=new`) on the machine's hardware GPU, with a
//! throwaway profile, in a process group of its own; a page's console reaches the driver as
//! DevTools events, over Chrome's pipe ([`cdp`]).
//!
//! **Every end is bounded and clean.** [`Browser::close`] (and its drop, so a panic too) closes
//! the pipe, kills the process group (`SIGKILL`: the profile is thrown away, so nothing is
//! lost), waits at most two seconds for the group to be gone (on Linux, reaping it: the process
//! that starts Chrome adopts its descendants, as tools/headless.py did), and removes the profile.
//! If the process that started Chrome dies first, even by `SIGKILL`, the pipe's end closes and
//! Chrome shuts down; its profile, in a directory of that process's own ([`profiles`]), is
//! removed at the next start. A supervisor that must kill the group records it, which the
//! [`Spawner`] is given the chance to do before Chrome runs.
//!
//! **A browser may be held to a policy** ([`Policy`]): every connection it makes, of its pages,
//! its workers and Chrome itself, goes through a proxy of the driver's ([`proxy`]), which lets
//! one through only to an exact scheme, host and port the policy grants.
//!
//! **No JavaScript crosses this API.** Scripts and observations are typed ([`script`]); the
//! driver's one function in the page reads what an observation names.
//!
//! Chrome is `WRELA_CHROME` if set, else the macOS app or the first of Linux's names on `PATH`
//! ([`chrome_binary`]); `WRELA_CHROME_ARGS` adds arguments, split as a shell splits them. A
//! Chrome that isn't there, or that offers only a software WebGPU adapter, fails loudly
//! ([`Tab::hardware_adapter`]): the driver never renders in software.

pub mod cdp;
mod keys;
pub mod proxy;
pub mod script;

pub use proxy::{Dest, Policy};

use cdp::{Connection, Event};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

/// How long Chrome may take to answer on its pipe.
pub const STARTUP: Duration = Duration::from_secs(15);

/// How long a killed group has to be seen gone.
const REAP: Duration = Duration::from_secs(2);

/// How long a DevTools call made while handling an event may take.
const EVENT_CALL: Duration = Duration::from_secs(10);

const LINUX_CHROMES: [&str; 4] =
    ["google-chrome-stable", "google-chrome", "chromium", "chromium-browser"];

/// Chrome's executable: `WRELA_CHROME`, else the macOS app, or the first of Linux's names on
/// `PATH`.
pub fn chrome_binary() -> PathBuf {
    chrome_binary_in(
        std::env::var_os("WRELA_CHROME"),
        std::env::consts::OS,
        std::env::var_os("PATH"),
    )
}

fn chrome_binary_in(set: Option<OsString>, os: &str, path: Option<OsString>) -> PathBuf {
    if let Some(chrome) = set.filter(|c| !c.is_empty()) {
        return PathBuf::from(chrome);
    }
    if os == "macos" {
        return PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
    }
    let dirs: Vec<PathBuf> = path.map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    LINUX_CHROMES
        .iter()
        .find_map(|name| dirs.iter().map(|d| d.join(name)).find(|p| p.is_file()))
        .unwrap_or_else(|| PathBuf::from(LINUX_CHROMES[0]))
}

/// The arguments a platform needs for its hardware WebGPU adapter. Linux Chrome keeps WebGPU
/// behind flags, and without Vulkan through ANGLE it offers only its software adapter. It also
/// reads cookies only after the system keyring answers, which can take 25 s; a test profile is
/// thrown away and needs no keyring.
pub fn platform_args(os: &str) -> &'static [&'static str] {
    if os == "linux" {
        &[
            "--enable-unsafe-webgpu",
            "--enable-features=Vulkan",
            "--use-angle=vulkan",
            "--ignore-gpu-blocklist",
            "--password-store=basic",
        ]
    } else {
        &[]
    }
}

/// Starts Chrome for the driver: the command it runs, and, once it has started, the process
/// group it leads. The tool host's records the group for its supervisor before it lets Chrome
/// run (`wrela tool --launch`).
pub trait Spawner {
    /// A command that runs `exe` with `args`, in a process group of its own once it's let go.
    fn command(&self, exe: &Path, args: &[OsString]) -> Result<Command, String>;
    /// `child` has started: its process group, once it's recorded and let go.
    fn started(&self, child: &mut Child) -> Result<i32, String>;
}

/// A [`Spawner`] for a process of no supervisor's: Chrome leads a new process group at once.
pub struct OwnGroup;

impl Spawner for OwnGroup {
    fn command(&self, exe: &Path, args: &[OsString]) -> Result<Command, String> {
        let mut c = Command::new(exe);
        c.args(args);
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut c, 0);
        Ok(c)
    }

    fn started(&self, child: &mut Child) -> Result<i32, String> {
        Ok(child.id() as i32)
    }
}

/// How to start a browser.
pub struct Launch {
    pub binary: PathBuf,
    /// The profile's directory, which the launch makes (it mustn't be there) and the close
    /// removes: in a directory of the process's own ([`profiles`]).
    pub profile: PathBuf,
    /// The window's size, for each page.
    pub window: (u32, u32),
    /// More arguments: `WRELA_CHROME_ARGS`'s.
    pub args: Vec<String>,
    pub startup: Duration,
    /// Where its connections may go, if it's held to a policy: every one goes through the
    /// driver's proxy ([`proxy`]).
    pub policy: Option<Policy>,
    /// Set to give up the start: the browser is ended at once.
    pub cancel: Option<Arc<AtomicBool>>,
    /// What the proxy names the browser as to the servers it reaches ([`proxy::SESSION_HEADER`]).
    pub session: Option<String>,
}

impl Launch {
    /// A launch of [`chrome_binary`] with `WRELA_CHROME_ARGS`, a profile at `profile` and a
    /// 1920×1080 window: an error if there's no Chrome there to run, or the arguments don't
    /// split.
    pub fn new(profile: PathBuf) -> Result<Launch, String> {
        let binary = chrome_binary();
        if !runnable(&binary) {
            return Err(format!(
                "Chrome isn't at {} (set WRELA_CHROME): the browser tests need Chrome stable",
                binary.display()
            ));
        }
        let extra = std::env::var("WRELA_CHROME_ARGS").unwrap_or_default();
        let args = shlex::split(&extra)
            .ok_or_else(|| format!("WRELA_CHROME_ARGS doesn't split as a shell splits: {extra}"))?;
        Ok(Launch {
            binary,
            profile,
            window: (1920, 1080),
            args,
            startup: STARTUP,
            policy: None,
            cancel: None,
            session: None,
        })
    }

    /// The command line: Chrome's flags, a proxy's if it has one, then the platform's and the
    /// launch's.
    fn argv(&self, proxy: Option<u16>) -> Vec<OsString> {
        let mut a: Vec<OsString> = [
            "--headless=new",
            // DevTools on fds 3 and 4: Chrome ends when their other ends close.
            "--remote-debugging-pipe",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            // No requests of Chrome's own: updates, sync, safe browsing's lists.
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-sync",
            "--disable-background-timer-throttling",
            "--disable-renderer-backgrounding",
            "--disable-backgrounding-occluded-windows",
            // A test page plays its voice without a click (test mode's `live`).
            "--autoplay-policy=no-user-gesture-required",
            // Timestamp queries at full resolution, not quantized to 100 µs.
            "--enable-webgpu-developer-features",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        a.push(format!("--user-data-dir={}", self.profile.display()).into());
        a.push(format!("--window-size={},{}", self.window.0, self.window.1).into());
        if let Some(port) = proxy {
            // Every connection through the proxy: no implicit bypass of loopback, local or
            // link-local addresses, and no QUIC, WebTransport or WebRTC UDP, which it can't
            // carry.
            a.push(format!("--proxy-server=http://127.0.0.1:{port}").into());
            a.push("--proxy-bypass-list=<-loopback>".into());
            a.push("--disable-quic".into());
            a.push("--disable-blink-features=WebTransport".into());
            // WebRTC's UDP (ICE, STUN, TURN) only through the proxy, which carries none.
            a.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into());
        }
        a.extend(platform_args(std::env::consts::OS).iter().map(OsString::from));
        a.extend(self.args.iter().map(OsString::from));
        a.push("about:blank".into());
        a
    }
}

/// Whether the file at `path` is one that runs: a regular file with an execute bit.
fn runnable(path: &Path) -> bool {
    #[cfg(unix)]
    let runs = |m: &std::fs::Metadata| {
        std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0
    };
    #[cfg(not(unix))]
    let runs = |_: &std::fs::Metadata| true;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && runs(&m))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether process `pid` still runs (as anyone's).
pub fn alive(pid: i32) -> bool {
    rustix::process::Pid::from_raw(pid).is_some_and(|p| {
        matches!(rustix::process::test_kill_process(p), Ok(()) | Err(rustix::io::Errno::PERM))
    })
}

/// This process's directory for its browsers' profiles, `<base>/wrela-chrome-<pid>`, made if
/// it isn't there. Those of processes that have died (killed before they could remove them) are
/// removed first: a profile outlives its process only until the next start.
pub fn profiles(base: &Path) -> Result<PathBuf, String> {
    sweep(base, "wrela-chrome-");
    let dir = base.join(format!("wrela-chrome-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// Removes each directory of `base` named `<prefix><pid>` or `<prefix><pid>-...` whose process
/// has died: what a killed process left.
pub fn sweep(base: &Path, prefix: &str) {
    let Ok(entries) = std::fs::read_dir(base) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(prefix) else { continue };
        let Ok(pid) = rest.split('-').next().unwrap_or("").parse::<i32>() else { continue };
        let ours = pid == std::process::id() as i32;
        // A link isn't followed: only a directory of a dead process's goes.
        let dir = e.file_type().is_ok_and(|t| t.is_dir());
        if !ours && dir && !alive(pid) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Makes this process the subreaper of its descendants (Linux): a process Chrome leaves when its
/// parent dies is this process's to reap, as tools/headless.py did, not an init's that may not.
#[cfg(target_os = "linux")]
fn adopt_descendants() -> Result<(), String> {
    static ADOPTED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    ADOPTED
        .get_or_init(|| {
            rustix::process::set_child_subreaper(Some(rustix::process::getpid())).map_err(|e| {
                format!("can't adopt Chrome's processes (PR_SET_CHILD_SUBREAPER): {e}")
            })
        })
        .clone()
}

/// Reaps every exited process of group `group` that is this process's child (Linux, where this
/// process adopts them, once it has started a browser: a granted program's orphans too): whether
/// none is left, exited or running. The tool host's `run` reaps its programs' groups with it.
#[cfg(target_os = "linux")]
pub fn reap_group(group: rustix::process::Pid) -> bool {
    loop {
        match rustix::process::waitpgid(group, rustix::process::WaitOptions::NOHANG) {
            // One reaped: look again.
            Ok(Some(_)) => {}
            // Some still run.
            Ok(None) => return false,
            // None of ours is in the group.
            Err(_) => return true,
        }
    }
}

/// Hands the pipe's ends to the child as its fds 3 (what it reads) and 4 (what it writes), as
/// `--remote-debugging-pipe` takes them.
#[cfg(unix)]
#[allow(unsafe_code)]
fn hand_pipes(c: &mut Command, read: std::os::fd::RawFd, write: std::os::fd::RawFd) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure runs in the child between `fork` and `exec`, and calls only `fcntl`
    // and `dup2`, which are async-signal-safe; it allocates nothing and touches no lock. The
    // ends are first copied above 10 (closed at `exec`), so neither `dup2` can overwrite the
    // other's source; `dup2` gives fds 3 and 4 without close-on-exec.
    unsafe {
        c.pre_exec(move || {
            let r = libc::fcntl(read, libc::F_DUPFD_CLOEXEC, 10);
            let w = libc::fcntl(write, libc::F_DUPFD_CLOEXEC, 10);
            if r < 0 || w < 0 || libc::dup2(r, 3) < 0 || libc::dup2(w, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// The process and profile of a browser: what [`Browser::close`] ends, also while it's still
/// starting.
struct Process {
    child: Mutex<Option<Child>>,
    group: i32,
    profile: PathBuf,
    ended: Mutex<Option<bool>>,
}

impl Process {
    /// Ends the group and removes the profile, once: whether the group is gone and the profile
    /// with it.
    fn end(&self) -> bool {
        let mut ended = lock(&self.ended);
        if let Some(clean) = *ended {
            return clean;
        }
        let mut child = lock(&self.child).take();
        let group = rustix::process::Pid::from_raw(self.group);
        let exited =
            |c: &mut Option<Child>| c.as_mut().is_none_or(|c| !matches!(c.try_wait(), Ok(None)));
        // The whole group, also what Chrome left when it exited first.
        if let Some(pid) = group {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        if let Some(c) = child.as_mut() {
            let _ = c.kill();
        }
        let since = Instant::now();
        let mut gone = false;
        while since.elapsed() < REAP {
            let reaped = exited(&mut child);
            #[cfg(target_os = "linux")]
            let reaped = reaped && group.is_none_or(reap_group);
            // macOS refuses (EPERM) to signal a group whose processes have all exited but not
            // been reaped: that group is gone too.
            let empty = group.is_none_or(|pid| {
                rustix::process::test_kill_process_group(pid)
                    .is_err_and(|e| e == rustix::io::Errno::SRCH || e == rustix::io::Errno::PERM)
            });
            if reaped && empty {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut removed = false;
        for _ in 0..50 {
            match std::fs::remove_dir_all(&self.profile) {
                Ok(()) => {
                    removed = true;
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    removed = true;
                    break;
                }
                // A process of the group writing as it died: once more.
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        *ended = Some(gone && removed);
        gone && removed
    }
}

/// A running Chrome.
pub struct Browser {
    inner: Arc<Inner>,
    tabs: Tabs,
}

struct Inner {
    conn: Connection,
    process: Process,
    proxy: Option<proxy::Proxy>,
    window: (u32, u32),
}

/// The tabs a browser's proxy tells of what it refused.
type Tabs = Arc<Mutex<Vec<std::sync::Weak<TabState>>>>;

impl Browser {
    /// Starts Chrome as `launch` says, through `spawner`, and connects to it. A browser that
    /// doesn't start is ended, and its profile removed, before this returns.
    pub fn launch(launch: Launch, spawner: &dyn Spawner) -> Result<Browser, String> {
        #[cfg(target_os = "linux")]
        adopt_descendants()?;
        let tabs: Tabs = Arc::default();
        let proxy = match &launch.policy {
            None => None,
            Some(policy) => {
                let told = tabs.clone();
                let refused: proxy::Refused = Arc::new(move |line| {
                    for t in lock(&told).iter().filter_map(std::sync::Weak::upgrade) {
                        t.line(line.clone());
                    }
                });
                Some(proxy::Proxy::start(policy.clone(), refused, launch.session.clone())?)
            }
        };
        std::fs::create_dir(&launch.profile)
            .map_err(|e| format!("Chrome's profile ({}): {e}", launch.profile.display()))?;
        if launch.policy.is_some() {
            // WebRTC sends UDP only through a proxy, and the proxy carries none: the profile's
            // own setting, which Chrome reads as it starts.
            let prefs = json!({"webrtc": {"ip_handling_policy": "disable_non_proxied_udp",
                                          "multiple_routes_enabled": false,
                                          "nonproxied_udp_enabled": false}});
            let default = launch.profile.join("Default");
            let written = std::fs::create_dir(&default)
                .and_then(|()| std::fs::write(default.join("Preferences"), prefs.to_string()));
            if let Err(e) = written {
                let _ = std::fs::remove_dir_all(&launch.profile);
                return Err(format!("Chrome's preferences: {e}"));
            }
        }
        let log_path = launch.profile.join("chrome.log");
        let started = (|| {
            let log = std::fs::File::create(&log_path).map_err(|e| format!("Chrome's log: {e}"))?;
            let err = log.try_clone().map_err(|e| format!("Chrome's log: {e}"))?;
            let (chrome_reads, to) = std::io::pipe().map_err(|e| format!("a pipe: {e}"))?;
            let (from, chrome_writes) = std::io::pipe().map_err(|e| format!("a pipe: {e}"))?;
            let mut c =
                spawner.command(&launch.binary, &launch.argv(proxy.as_ref().map(|p| p.port)))?;
            c.stdin(std::process::Stdio::null()).stdout(log).stderr(err);
            #[cfg(unix)]
            {
                use std::os::fd::AsRawFd;
                hand_pipes(&mut c, chrome_reads.as_raw_fd(), chrome_writes.as_raw_fd());
            }
            let mut child = c
                .spawn()
                .map_err(|e| format!("can't run Chrome ({}): {e}", launch.binary.display()))?;
            // Chrome's ends are its own now: ours close, so its exit ends the pipe.
            drop((chrome_reads, chrome_writes));
            let group = match spawner.started(&mut child) {
                Ok(g) => g,
                Err(why) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(why);
                }
            };
            Ok((child, group, to, from))
        })();
        let (child, group, to, from) = match started {
            Ok(s) => s,
            Err(why) => {
                let _ = std::fs::remove_dir_all(&launch.profile);
                return Err(why);
            }
        };
        let process = Process {
            child: Mutex::new(Some(child)),
            group,
            profile: launch.profile.clone(),
            ended: Mutex::new(None),
        };
        let connected = Connection::start(to, from).and_then(|conn| {
            let deadline = Instant::now() + launch.startup;
            loop {
                let ask = (Instant::now() + Duration::from_millis(250)).min(deadline);
                match conn.call(None, "Browser.getVersion", json!({}), ask) {
                    Ok(_) => return Ok(conn),
                    Err(why) if conn.ended().is_some() => return Err(why),
                    Err(_) if Instant::now() >= deadline => {
                        return Err(format!("no DevTools answer after {:?}", launch.startup));
                    }
                    Err(_) if launch.cancel.as_ref().is_some_and(|c| c.load(Ordering::Acquire)) => {
                        return Err("its start was given up".into());
                    }
                    Err(_) => {}
                }
            }
        });
        let conn = match connected {
            Ok(c) => c,
            Err(why) => {
                process.end();
                let log = std::fs::read_to_string(&log_path).unwrap_or_default();
                let tail: Vec<&str> = log.lines().rev().take(12).collect();
                let tail: Vec<&str> = tail.into_iter().rev().collect();
                if let Some(p) = &proxy {
                    p.close(Duration::from_secs(1));
                }
                return Err(format!(
                    "Chrome ({}) didn't start: {why}\n{}",
                    launch.binary.display(),
                    tail.join("\n")
                ));
            }
        };
        let inner = Arc::new(Inner { conn, process, proxy, window: launch.window });
        Ok(Browser { inner, tabs })
    }

    /// Whether Chrome still runs and its connection is open.
    pub fn alive(&self) -> bool {
        if self.inner.conn.ended().is_some() {
            return false;
        }
        lock(&self.inner.process.child).as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }

    /// The process group Chrome leads.
    pub fn group(&self) -> i32 {
        self.inner.process.group
    }

    /// Opens `url` in a new window of its own, by `deadline`: its tab. With a size, its viewport
    /// is that size at a device scale of 1; with a console file, each console line of its page
    /// and workers is appended there as it comes; with `done`, what the page's results/DONE
    /// holds once its server has kept it.
    pub fn open(&self, url: &str, options: TabOptions, deadline: Instant) -> Result<Tab, String> {
        let conn = &self.inner.conn;
        let made = conn.call(
            None,
            "Target.createTarget",
            json!({"url": "about:blank", "newWindow": true}),
            deadline,
        )?;
        let target = made["targetId"].as_str().ok_or("Target.createTarget gave no target")?;
        let state = Arc::new(TabState::new(options.console.as_deref())?);
        lock(&self.tabs).push(Arc::downgrade(&state));
        let mut tab = Tab {
            inner: self.inner.clone(),
            target: target.to_string(),
            session: String::new(),
            state: state.clone(),
            done: options.done.clone(),
            thread: None,
            tag: 0,
            closed: AtomicBool::new(false),
        };
        let attached = conn.call(
            None,
            "Target.attachToTarget",
            json!({"targetId": target, "flatten": true}),
            deadline,
        )?;
        let session =
            attached["sessionId"].as_str().ok_or("Target.attachToTarget gave no session")?;
        tab.session = session.to_string();
        let (to, events) = mpsc::channel();
        tab.tag = conn.route(session, to);
        let (inner, st) = (Arc::downgrade(&self.inner), state.clone());
        let thread = std::thread::Builder::new()
            .name("wrela-chrome-tab".into())
            .spawn(move || {
                while let Ok(e) = events.recv() {
                    let Some(inner) = inner.upgrade() else { break };
                    handle(&inner, &st, &e);
                }
            })
            .map_err(|e| format!("can't start the tab's events thread: {e}"))?;
        tab.thread = Some(thread);
        let s = Some(session);
        conn.call(s, "Runtime.enable", json!({}), deadline)?;
        conn.call(s, "Log.enable", json!({}), deadline)?;
        conn.call(s, "Inspector.enable", json!({}), deadline)?;
        conn.call(
            s,
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
            deadline,
        )?;
        if let Some((w, h)) = options.size {
            tab.resize(w, h, deadline)?;
        }
        let went = conn.call(s, "Page.navigate", json!({"url": url}), deadline)?;
        if let Some(why) = went.get("errorText").and_then(Value::as_str) {
            return Err(format!("Chrome couldn't open {url}: {why}"));
        }
        Ok(tab)
    }

    /// Closes the DevTools pipe: every call waiting on it fails at once, none can be made, and
    /// Chrome, its end closed, shuts down. [`Browser::close`] then ends what's left.
    pub fn disconnect(&self) {
        self.inner.conn.close(Duration::from_secs(1));
    }

    /// [`Browser::disconnect`], waiting for nothing: for a caller that mustn't block.
    pub fn cut(&self) {
        self.inner.conn.cut();
    }

    /// Ends Chrome: its pipe, then its process group, then its profile and its proxy. Whether it
    /// ended cleanly (the group gone, the profile removed, every thread joined) within its
    /// bounded grace.
    pub fn close(&self) -> bool {
        let closed = self.inner.conn.close(Duration::from_secs(1));
        let ended = self.inner.process.end();
        // Chrome's end of the pipe has closed with it.
        let read = self.inner.conn.join_reader(Duration::from_secs(2));
        let proxied = self.inner.proxy.as_ref().is_none_or(|p| p.close(Duration::from_secs(2)));
        closed && ended && read && proxied
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.close();
    }
}

/// What the page put in results/DONE, once its server has kept it: its text, or why it can't
/// be read whole (it's past the most a result is read as).
pub type Done = Arc<dyn Fn() -> Option<Result<String, String>> + Send + Sync>;

/// How to open a tab.
#[derive(Clone, Default)]
pub struct TabOptions {
    pub size: Option<(u32, u32)>,
    pub console: Option<PathBuf>,
    pub done: Option<Done>,
}

/// What a tab's events tell: its console, the program's printed lines, whether it crashed, and
/// its viewport's size.
struct TabState {
    seen: Mutex<Seen>,
    changed: Condvar,
    console: Option<Mutex<std::fs::File>>,
}

#[derive(Default)]
struct Seen {
    console: Vec<String>,
    printed: Vec<String>,
    /// The lines past [`MOST_LINES`] that weren't kept, of the console and of what was printed.
    console_dropped: usize,
    printed_dropped: usize,
    crashed: bool,
    size: Option<(u32, u32)>,
}

/// The most console lines a tab keeps (the file gets them all): past them, an observation of
/// the console, or a wait for a printed line, fails, saying so.
const MOST_LINES: usize = 100_000;

impl TabState {
    fn new(console: Option<&Path>) -> Result<TabState, String> {
        let console = match console {
            None => None,
            Some(p) => Some(Mutex::new(
                std::fs::File::create(p).map_err(|e| format!("{}: {e}", p.display()))?,
            )),
        };
        Ok(TabState { seen: Mutex::new(Seen::default()), changed: Condvar::new(), console })
    }

    fn line(&self, text: String) {
        if let Some(f) = &self.console {
            let mut f = lock(f);
            let _ = f.write_all(text.as_bytes());
            let _ = f.write_all(b"\n");
            let _ = f.flush();
        }
        let mut seen = lock(&self.seen);
        if let Some(p) = text.strip_prefix("wrela: ") {
            if seen.printed.len() < MOST_LINES {
                seen.printed.push(p.to_string());
            } else {
                seen.printed_dropped += 1;
            }
        }
        if seen.console.len() < MOST_LINES {
            seen.console.push(text);
        } else {
            seen.console_dropped += 1;
        }
        drop(seen);
        self.changed.notify_all();
    }

    fn change(&self, f: impl FnOnce(&mut Seen)) {
        f(&mut lock(&self.seen));
        self.changed.notify_all();
    }

    /// Waits at most `wait` for anything new.
    fn wait(&self, wait: Duration) {
        let seen = lock(&self.seen);
        let _ = self.changed.wait_timeout(seen, wait);
    }
}

/// A console message's text: its arguments, each as the console shows it, between spaces.
fn console_text(params: &Value) -> String {
    let args = params["args"].as_array().cloned().unwrap_or_default();
    let parts: Vec<String> = args
        .iter()
        .map(|a| match &a["value"] {
            Value::String(s) => s.clone(),
            Value::Null => {
                a["description"].as_str().or(a["type"].as_str()).unwrap_or("").to_string()
            }
            v => v.to_string(),
        })
        .collect();
    parts.join(" ")
}

/// Handles event `e` of a tab's page or one of its workers.
fn handle(inner: &Inner, state: &TabState, e: &Event) {
    let conn = &inner.conn;
    let soon = || Instant::now() + EVENT_CALL;
    match e.method.as_str() {
        "Runtime.consoleAPICalled" => state.line(console_text(&e.params)),
        "Runtime.exceptionThrown" => {
            let d = &e.params["exceptionDetails"];
            let text = d["exception"]["description"].as_str().or(d["text"].as_str());
            state.line(format!("Uncaught {}", text.unwrap_or("an exception")));
        }
        "Log.entryAdded" => {
            let entry = &e.params["entry"];
            // A worker's console is its own session's, which the page's log repeats.
            if entry["source"] != "worker" {
                state.line(entry["text"].as_str().unwrap_or("").to_string());
            }
        }
        "Inspector.targetCrashed" => state.change(|seen| seen.crashed = true),
        "Target.attachedToTarget" => {
            // A worker (or a frame of another process) the page started, paused until its
            // console is heard too.
            let Some(child) = e.params["sessionId"].as_str() else { return };
            let c = Some(child);
            let _ = conn.call(c, "Runtime.enable", json!({}), soon());
            let _ = conn.call(c, "Log.enable", json!({}), soon());
            let _ = conn.call(
                c,
                "Target.setAutoAttach",
                json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
                soon(),
            );
            let _ = conn.call(c, "Runtime.runIfWaitingForDebugger", json!({}), soon());
        }
        "Target.detachedFromTarget" => {
            if let Some(child) = e.params["sessionId"].as_str() {
                conn.unroute_session(child);
            }
        }
        _ => {}
    }
}

/// A page in a window of its own.
pub struct Tab {
    inner: Arc<Inner>,
    target: String,
    session: String,
    state: Arc<TabState>,
    done: Option<Done>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The tag of its sessions' route.
    tag: u64,
    closed: AtomicBool,
}

impl Tab {
    /// Calls `method` with `params` in the tab's page, by `deadline`.
    pub fn call(&self, method: &str, params: Value, deadline: Instant) -> Result<Value, String> {
        self.inner.conn.call(Some(&self.session), method, params, deadline)
    }

    /// Calls `method` with `params` on the browser, by `deadline`.
    pub fn call_browser(
        &self,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, String> {
        self.inner.conn.call(None, method, params, deadline)
    }

    /// Evaluates the driver's own `expression` in the page, awaiting a promise: its value, or why
    /// there is none (the page threw, or has no context to evaluate in yet).
    pub(crate) fn evaluate(&self, expression: &str, deadline: Instant) -> Result<Value, String> {
        let r = self.call(
            "Runtime.evaluate",
            json!({"expression": expression, "returnByValue": true, "awaitPromise": true}),
            deadline,
        )?;
        if let Some(d) = r.get("exceptionDetails") {
            let why = d["exception"]["description"].as_str().or(d["text"].as_str());
            return Err(format!("the page threw: {}", why.unwrap_or("an exception")));
        }
        Ok(r["result"]["value"].clone())
    }

    /// Sets the viewport to `width` × `height` at a device scale of 1.
    pub fn resize(&self, width: u32, height: u32, deadline: Instant) -> Result<(), String> {
        self.call(
            "Emulation.setDeviceMetricsOverride",
            json!({"width": width, "height": height, "deviceScaleFactor": 1, "mobile": false}),
            deadline,
        )?;
        self.state.change(|seen| seen.size = Some((width, height)));
        Ok(())
    }

    /// The viewport's size: what [`Tab::resize`] set, or the window's.
    pub fn size(&self) -> (u32, u32) {
        lock(&self.state.seen).size.unwrap_or(self.inner.window)
    }

    /// Whether the page offers a hardware WebGPU adapter: an error saying why not, if not.
    pub fn hardware_adapter(&self, deadline: Instant) -> Result<(), String> {
        let found = self.evaluate(script::ADAPTER, deadline)?;
        match found.as_str() {
            Some("") => Ok(()),
            Some(why) => Err(format!("{why}: the browser tests need a hardware GPU")),
            None => Err("the page didn't say what adapter it has".into()),
        }
    }

    /// Every console line of the page and its workers so far: an error if there were more than
    /// a tab keeps (100 000), rather than some of them.
    pub fn console(&self) -> Result<Vec<String>, String> {
        let seen = lock(&self.state.seen);
        if seen.console_dropped > 0 {
            return Err(format!(
                "the console had more than {MOST_LINES} lines: {} more weren't kept",
                seen.console_dropped
            ));
        }
        Ok(seen.console.clone())
    }

    /// The lines the program printed (`std::io::print`, which the browser runtime shows on the
    /// console as `wrela: <line>`): an error if there were more than a tab keeps.
    pub fn printed(&self) -> Result<Vec<String>, String> {
        let seen = lock(&self.state.seen);
        if seen.printed_dropped > 0 {
            return Err(format!(
                "the program printed more than {MOST_LINES} lines: {} more weren't kept",
                seen.printed_dropped
            ));
        }
        Ok(seen.printed.clone())
    }

    /// What the page put in `results/DONE`, once its server has kept it (or why it can't be
    /// read whole): `None` until then, and for a tab opened with nothing to tell it.
    pub fn done(&self) -> Option<Result<String, String>> {
        self.done.as_ref().and_then(|d| d())
    }

    /// Whether the tab was opened with what tells it its page's result.
    pub fn knows_done(&self) -> bool {
        self.done.is_some()
    }

    /// Why the tab's connection to Chrome ended, if it has: the browser closed, or Chrome
    /// exited.
    pub fn ended(&self) -> Option<String> {
        self.inner.conn.ended()
    }

    /// Whether the page's process crashed.
    pub fn crashed(&self) -> bool {
        lock(&self.state.seen).crashed
    }

    /// Waits at most `wait` for anything new to be seen.
    pub(crate) fn wait_for_change(&self, wait: Duration) {
        self.state.wait(wait);
    }

    /// The page's screen, as a PNG at most `most` bytes long: the viewport at its size.
    pub fn screenshot(&self, most: usize, deadline: Instant) -> Result<Vec<u8>, String> {
        let (w, h) = self.size();
        let shot = self.call(
            "Page.captureScreenshot",
            json!({"format": "png", "fromSurface": true, "captureBeyondViewport": false,
                   "clip": {"x": 0, "y": 0, "width": w, "height": h, "scale": 1}}),
            deadline,
        )?;
        let data = shot["data"].as_str().ok_or("Page.captureScreenshot gave no data")?;
        if data.len() / 4 * 3 > most {
            return Err(format!("the screenshot is more than {most} bytes"));
        }
        use base64::Engine;
        let png = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| format!("the screenshot's base64: {e}"))?;
        if png.len() > most {
            return Err(format!("the screenshot is more than {most} bytes"));
        }
        Ok(png)
    }

    /// Closes the tab's window by `deadline`.
    pub fn close(&self, deadline: Instant) -> Result<(), String> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let closed = self.inner.conn.call(
            None,
            "Target.closeTarget",
            json!({"targetId": self.target}),
            deadline,
        );
        self.inner.conn.unroute(self.tag);
        closed.map(|_| ())
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        let _ = self.close(Instant::now() + Duration::from_secs(5));
        if let Some(t) = self.thread.take() {
            // Unrouted, its channel ends, and the thread with it, once an event it's handling
            // is done.
            let since = Instant::now();
            while !t.is_finished() && since.elapsed() < EVENT_CALL {
                std::thread::sleep(Duration::from_millis(2));
            }
            if t.is_finished() {
                let _ = t.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_is_found_by_platform_and_linux_gets_its_gpu_flags() {
        let set = chrome_binary_in(Some("/x/chrome".into()), "linux", None);
        assert_eq!(set, Path::new("/x/chrome"));
        assert!(chrome_binary_in(None, "macos", None).starts_with("/Applications/"));
        let none = chrome_binary_in(None, "linux", Some("/nowhere".into()));
        assert_eq!(none, Path::new("google-chrome-stable"));
        assert!(platform_args("linux").contains(&"--use-angle=vulkan"));
        assert!(platform_args("linux").contains(&"--password-store=basic"));
        assert!(platform_args("macos").is_empty());
    }

    #[test]
    fn a_launch_names_its_pipe_profile_window_and_a_policys_proxy() {
        let l = Launch {
            binary: "chrome".into(),
            profile: "/p".into(),
            window: (640, 480),
            args: vec!["--x".into()],
            startup: STARTUP,
            policy: None,
            cancel: None,
            session: None,
        };
        let argv = |proxy: Option<u16>| -> Vec<String> {
            l.argv(proxy).iter().map(|a| a.to_string_lossy().into_owned()).collect()
        };
        let a = argv(None);
        for flag in [
            "--headless=new",
            "--remote-debugging-pipe",
            "--user-data-dir=/p",
            "--window-size=640,480",
            "--x",
        ] {
            assert!(a.contains(&flag.to_string()), "{flag}: {a:?}");
        }
        assert!(
            !a.iter().any(|x| x.starts_with("--proxy") || x.starts_with("--remote-debugging-port")),
            "{a:?}"
        );
        assert_eq!(a.last().map(String::as_str), Some("about:blank"));
        let a = argv(Some(4321));
        for flag in [
            "--proxy-server=http://127.0.0.1:4321",
            "--proxy-bypass-list=<-loopback>",
            "--disable-quic",
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
        ] {
            assert!(a.contains(&flag.to_string()), "{flag}: {a:?}");
        }
    }

    #[test]
    fn a_dead_processs_profiles_are_swept_and_a_living_ones_kept() {
        let base = std::env::temp_dir().join(format!("wrela-sweep-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("base");
        // A child that has exited: its pid is no process's now.
        let mut gone = Command::new("true").spawn().expect("true");
        gone.wait().expect("wait");
        let dead = base.join(format!("wrela-chrome-{}", gone.id()));
        let living = base.join(format!("wrela-chrome-{}", std::process::id()));
        let parent = base.join(format!("wrela-chrome-{}-7", std::os::unix::process::parent_id()));
        let other = base.join("wrela-chrome-notapid");
        for d in [&dead, &living, &parent, &other] {
            std::fs::create_dir_all(d.join("browser-1")).expect("a profile");
        }
        let ours = profiles(&base).expect("ours");
        assert_eq!(ours, living);
        assert!(!dead.exists(), "a dead process's profiles are swept");
        assert!(living.exists() && parent.exists() && other.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn on_linux_the_driver_adopts_and_reaps_chromes_group() {
        use std::os::unix::process::CommandExt;
        adopt_descendants().expect("a subreaper");
        let mut c = Command::new("sh")
            .args(["-c", "sleep 30 & exit 0"])
            .process_group(0)
            .spawn()
            .expect("sh");
        let group = rustix::process::Pid::from_raw(c.id() as i32).expect("a group");
        c.wait().expect("sh exits");
        // Its child is ours now; killed, it's reaped with the group.
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        let since = Instant::now();
        while !reap_group(group) {
            assert!(since.elapsed() < Duration::from_secs(5), "the orphan was reaped");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_console_message_reads_as_the_console_shows_it() {
        let p = json!({"args": [{"type": "string", "value": "wrela: text"}, {"type": "number", "value": 3},
                                 {"type": "undefined"}, {"type": "object", "description": "Array(2)"}]});
        assert_eq!(console_text(&p), "wrela: text 3 undefined Array(2)");
    }
}
