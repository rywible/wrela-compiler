//! `std::tool::browser` (language.md §15.5) on the tool host, in real Chrome stable on the
//! hardware GPU: a tool serves pages from its scratch root (`std::tool::serve`), opens them, acts
//! on them with typed scripts (trusted clicks and keys, an input method, permissions, focus) and
//! reads what they show (texts, counts, attributes, the focus, the location, the console, the
//! clipboard, the test-mode result, the screen). Its pages reach only its own servers: a fetch to
//! another local port, from the page or its worker, and one to a host it isn't granted, are
//! refused. The clipboard is read only once it's granted. A page that never reports fails its
//! wait at the wait's timeout, and the request at its deadline, in bounded time. However the tool
//! ends, a trap with a browser open among them, no Chrome process and no profile is left.

use crate::tool::{printer, run, scratch, tool_build};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use wrela_host::tool::{Caps, End, Ended, Grants, Signal, Testing};

/// A tool that serves its scratch root, opens the page its second argument names there, and does
/// what its first says:
/// - `act`: types into the form and clicks its button, then prints what it observes;
/// - `refused`: waits until the page has tried what it may not reach, and prints its console;
/// - `clipboard`: reads the clipboard before and after a script grants it and clicks the copy
///   button;
/// - `done`: waits for the page's result, and prints it;
/// - `never`: waits for a result that never comes, by its wait's timeout, then by the request's
///   deadline;
/// - `trap`: panics with the browser open.
///
/// Each failure prints why, and the tool exits 1.
const BROWSER_TOOL: &str = r##"
use std::input::{Key, Mods}
use std::io::print
use std::tool::{Call, Done, args}
use std::tool::blob
use std::tool::blob::Data
use std::tool::browser
use std::tool::browser::{Element, Match, Permission, Script, Until}
use std::tool::files
use std::tool::serve
use std::tool::time

/// Prints how a run and a close, asked at once, ended: the run's first.
fn both(r: take Call<Done>, c: take Call<Done>) {
    match take r.wait() {
        Ok(_) => print("run: done"),
        Err(e) => print(f"run: {e.message()}"),
    }
    match take c.wait() {
        Ok(_) => print("close: closed"),
        Err(e) => print(f"close: {e.message()}"),
    }
}

pub fn tool() -> u32 {
    let given = args()
    let what = given[0].clone()
    let page = given[1].clone()
    let root = if given.len() > 2 { given[2].clone() } else { String::from("scratch") }
    let started = match serve::start(root.as_str()).wait() {
        Ok(x) => take x,
        Err(e) => {
            print(f"serve: {e.message()}")
            return 1
        },
    }
    let url = f"{started.origin.as_str()}/{page.as_str()}"
    let b = match browser::open(url.as_str(), width: 320, height: 240).wait() {
        Ok(b) => b,
        Err(e) => {
            print(f"open: {e.message()}")
            return 1
        },
    }
    print("opened")
    if what.as_str() == "trap" {
        panic("a trap with a browser open")
    }
    var script = Script::new()
    if what.as_str() == "act" {
        script = Script::new()
            .wait(Until::title(Match::is("Form")))
            .click(Element::css("#name"))
            .wait(Until::active("input"))
            .press(Key::A)
            .press(Key::B, Mods { shift: true })
            .insert_text("é")
            .wait(Until::title(Match::is("typed aBé")))
            .click(Element::css("button").with_text("Go"))
            .wait(Until::exists(Element::css("#out").with_text("clicked")))
    }
    if what.as_str() == "refused" {
        script = Script::new().wait(Until::title(Match::starts("tried")))
    }
    if what.as_str() == "whole" {
        script = Script::new()
            .wait(Until::count(Element::css("li"), 4097))
            .grant(Permission::Clipboard)
            .emulate_focus()
            .click(Element::css("button").with_text("Copy"))
            .wait(Until::clipboard(Match::ends("!")))
    }
    if what.as_str() == "closing" {
        // A wait of 600 s, and the browser closed while it waits.
        let r = browser::run(b, Script::new().wait(Until::done(), timeout: 600.0), timeout: 600.0)
        let _ = time::after(0.3).wait()
        both(take r, browser::close(b))
        return 0
    }
    if what.as_str() == "rejected" {
        script = Script::new().wait(Until::done(), timeout: 2.0)
    }
    if what.as_str() == "busy" {
        // Three more browsers, and a wait of 600 s on each of the four: every long worker.
        var all: Vec<browser::Browser> = Vec::new()
        all.push(b)
        for _ in 0..3 {
            let Ok(b2) = browser::open(url.as_str(), width: 320, height: 240).wait() else { return 91 }
            all.push(b2)
        }
        var waits: Vec<Call<Done>> = Vec::new()
        for x in all {
            waits.push(browser::run(x, Script::new().wait(Until::done(), timeout: 600.0), timeout: 600.0))
        }
        let _ = time::after(1.0).wait()
        // The first is closed, its close queued behind the four waits.
        let c = browser::close(b)
        var first = waits.remove(0)
        both(take first, take c)
        return 0
    }
    if what.as_str() == "gather" {
        script = Script::new().wait(Until::title(Match::starts("gathered")))
    }
    if what.as_str() == "two" {
        // A second browser, on the same server, whose page puts its result.
        let Ok(b2) = browser::open(f"{started.origin.as_str()}/done.html".as_str()).wait() else { return 91 }
        match browser::run(b2, Script::new().wait(Until::done())).wait() {
            Ok(_) => print("the second's page is done"),
            Err(e) => print(f"the second: {e.message()}"),
        }
        match browser::results(b).wait() {
            Ok(Some(t)) => print(f"the first's result: {t.as_str()}"),
            Ok(None) => print("the first has no result"),
            Err(e) => print(f"results: {e.message()}"),
        }
        script = Script::new().wait(Until::done(), timeout: 1.0)
    }
    if what.as_str() == "clipboard" {
        match browser::clipboard(b).wait() {
            Ok(t) => print(f"clipboard before: {t.as_str()}"),
            Err(_) => print("clipboard before: refused"),
        }
        script = Script::new()
            .grant(Permission::Clipboard)
            .emulate_focus()
            .click(Element::css("button").with_text("Copy"))
            .wait(Until::clipboard(Match::is("copied!")))
    }
    if what.as_str() == "done" || what.as_str() == "delayed" || what.as_str() == "huge" {
        script = Script::new().wait(Until::done())
    }
    if what.as_str() == "never" {
        script = Script::new().wait(Until::done(), timeout: 0.3)
    }
    if let Err(e) = browser::run(b, script).wait() {
        print(f"run: {e.message()}")
        if what.as_str() == "rejected" {
            match files::read(f"{root.as_str()}/results/DONE".as_str()).wait() {
                Ok(_) => print("kept a DONE"),
                Err(_) => print("kept no DONE"),
            }
            return 0
        }
        if what.as_str() == "two" {
            return 0
        }
        if what.as_str() == "huge" {
            match browser::results(b).wait() {
                Ok(Some(t)) => print(f"results: {t.as_str()}"),
                Ok(None) => print("results: none"),
                Err(e) => print(f"results: {e.message()}"),
            }
            return 0
        }
        if what.as_str() != "never" {
            return 1
        }
        // The request's own deadline, sooner than its wait's.
        let late = browser::run(b, Script::new().wait(Until::done(), timeout: 30.0), timeout: 0.5)
        if let Err(e) = take late.wait() {
            print(f"deadline: {e.message()}")
        }
        return 0
    }
    if what.as_str() == "act" {
        let Ok(title) = browser::title(b).wait() else { return 2 }
        print(f"title {title.as_str()}")
        let Ok(items) = browser::texts(b, "li").wait() else { return 2 }
        print(f"items {items.len()} {items[0].as_str()} {items[1].as_str()}")
        let Ok(count) = browser::count(b, Element::css("li").with_text("two")).wait() else { return 2 }
        print(f"count {count}")
        let Ok(Some(href)) = browser::attribute(b, Element::css("#next"), "href").wait() else { return 2 }
        print(f"href {href.as_str()}")
        let Ok(Some(focused)) = browser::active(b).wait() else { return 2 }
        print(f"active {focused.tag.as_str()} {focused.id.as_str()}")
        let Ok(location) = browser::location(b).wait() else { return 2 }
        print(f"page {Match::ends("/form.html").matches(location.as_str())}")
        let capture = match browser::capture(b).wait() {
            Ok(c) => take c,
            Err(e) => {
                print(f"capture: {e.message()}")
                return 1
            },
        }
        let Data::Blob(png) = capture.png else { return 3 }
        let Ok(head) = blob::read(png, 0, 8).wait() else { return 2 }
        print(f"capture {capture.width}x{capture.height} png {head.len() == 8 && head[1] == 80 && head[2] == 78 && head[3] == 71}")
    }
    if what.as_str() == "refused" || what.as_str() == "act" || what.as_str() == "gather" {
        let Ok(lines) = browser::console(b).wait() else { return 2 }
        for line in lines {
            print(f"console {line.as_str()}")
        }
        let Ok(title) = browser::title(b).wait() else { return 2 }
        print(f"title {title.as_str()}")
    }
    if what.as_str() == "clipboard" {
        let Ok(t) = browser::clipboard(b).wait() else { return 2 }
        print(f"clipboard after: {t.as_str()}")
    }
    if what.as_str() == "whole" {
        match browser::texts(b, "li").wait() {
            Ok(items) => print(f"items {items.len()}, the last {items[items.len() - 1].as_str()}"),
            Err(e) => print(f"texts: {e.message()}"),
        }
        match browser::clipboard(b).wait() {
            Ok(t) => print(f"clipboard {t.len()} bytes"),
            Err(e) => print(f"clipboard: {e.message()}"),
        }
    }
    if what.as_str() == "delayed" {
        // The result the browser saw is the one its server kept: on disk before close.
        match files::read(f"{root.as_str()}/results/DONE".as_str()).wait() {
            Ok(bytes) => print(f"kept {bytes.len()} bytes"),
            Err(e) => print(f"kept nothing: {e.message()}"),
        }
    }
    if what.as_str() == "done" {
        let Ok(Some(done)) = browser::results(b).wait() else { return 2 }
        print(f"done {done.as_str()}")
    }
    if let Err(e) = browser::close(b).wait() {
        print(f"close: {e.message()}")
        return 1
    }
    print("closed")
    0
}
"##;

const FORM: &str = r##"<!doctype html>
<title>Form</title>
<input id="name"><button onclick="document.getElementById('out').textContent = 'clicked'">Go</button>
<p id="out"></p>
<ul><li>one</li><li>two</li></ul>
<a id="next" href="#next">Next</a>
<script>
document.getElementById("name").addEventListener("input", (e) => { document.title = "typed " + e.target.value; });
console.log("ready");
</script>
"##;

const CLIPBOARD: &str = r#"<!doctype html>
<title>Clipboard</title>
<button onclick="navigator.clipboard.writeText('copied!')">Copy</button>
"#;

const DONE: &str = r#"<!doctype html>
<title>Done</title>
<script>fetch("results/DONE", { method: "PUT", body: "ok" });</script>
"#;

/// A page whose result is "ok", a MiB of spaces, then "failed".
const HUGE: &str = r#"<!doctype html>
<title>Huge</title>
<script>fetch("results/DONE", { method: "PUT", body: "ok" + " ".repeat(1 << 20) + "failed" });</script>
"#;

const NEVER: &str = "<!doctype html><title>Never</title><p>This page never reports.</p>\n";

/// A page that gathers ICE candidates against a STUN and a TURN server at `DATAGRAMS`, a UDP
/// port of this machine, and says in its title when it has, or 5 s have passed.
const GATHERING: &str = r#"<!doctype html>
<title>Gathering</title>
<script>
const pc = new RTCPeerConnection({ iceServers: [
  { urls: "stun:127.0.0.1:DATAGRAMS" },
  { urls: "turn:127.0.0.1:DATAGRAMS?transport=udp", username: "u", credential: "c" },
] });
pc.createDataChannel("d");
const kinds = [];
pc.onicecandidate = (e) => { if (e.candidate) kinds.push(e.candidate.type); };
const gathered = new Promise((done) => {
  pc.onicegatheringstatechange = () => { if (pc.iceGatheringState === "complete") done(); };
  setTimeout(done, 5000);
});
pc.createOffer().then((o) => pc.setLocalDescription(o));
gathered.then(() => { document.title = "gathered " + kinds.join(" "); });
</script>
"#;

/// A page of 4,097 items, and a button that copies 20,000 bytes, the last a `!`.
const WHOLE: &str = r#"<!doctype html>
<title>Whole</title>
<ul id="items"></ul>
<button onclick="navigator.clipboard.writeText('x'.repeat(19999) + '!')">Copy</button>
<script>
const list = document.getElementById("items");
for (let i = 0; i < 4097; i++) {
  const li = document.createElement("li");
  li.textContent = "item " + i;
  list.appendChild(li);
}
</script>
"#;

/// A page that tries, from itself and from its worker, what it may not reach: a plain fetch, a
/// WebSocket, a secure WebSocket and WebTransport to listening ports of this machine that aren't
/// its server's (`OTHER` for TCP, `DATAGRAMS` for UDP), the same to another port of a host the
/// tool is granted (`localhost`, its port 443 alone), and a fetch to a host it isn't granted.
/// Its title says what each did.
const REFUSING: &str = r##"<!doctype html>
<title>Refusing</title>
<script>
const attempts = `
  var fetched = (u) => fetch(u).then(() => "reached", () => "refused");
  var socket = (u) => new Promise((done) => {
    try {
      const w = new WebSocket(u);
      w.onopen = () => done("reached");
      w.onerror = () => done("refused");
    } catch (e) { done("refused"); }
  });
  var transport = (u) => {
    try { return new WebTransport(u).ready.then(() => "reached", () => "refused"); }
    catch (e) { return Promise.resolve("refused"); }
  };
  // An attempt that neither fails nor succeeds in 5 s says so.
  var bounded = (p) => Promise.race([p, new Promise((done) => setTimeout(() => done("hung"), 5000))]);
  var tried = () => Promise.all([
    fetched("http://127.0.0.1:OTHER/x"), fetched("https://example.com/"),
    socket("ws://127.0.0.1:OTHER/"), socket("wss://127.0.0.1:OTHER/"),
    transport("https://127.0.0.1:DATAGRAMS/"),
    socket("ws://localhost:OTHER2/"), socket("wss://localhost:OTHER2/"),
    transport("https://localhost:DATAGRAMS2/"),
  ].map(bounded));
`;
// Declared with `var`, so the page's own scope has them too.
eval(attempts);
const worker = new Worker(URL.createObjectURL(new Blob(
  [attempts + "tried().then((r) => postMessage(r.join(' ')));"], { type: "text/javascript" })));
const fromWorker = new Promise((done) => {
  worker.onmessage = (e) => done(e.data);
  worker.onerror = (e) => done("the worker failed: " + e.message);
  setTimeout(() => done("the worker hung"), 8000);
});
Promise.all([tried().then((r) => r.join(" ")), fromWorker])
  .then((r) => { document.title = "tried " + r.join(" | "); }, (e) => { document.title = "tried: " + e; });
</script>
"##;

/// The grants a browser tool gets: its scratch root, and Chrome.
fn grants(root: &Path) -> Grants {
    Grants { browser: true, ..Grants::default() }.root("scratch", root, true)
}

/// Runs the browser tool doing `what` on `page` (written to its scratch root as `name`), with
/// `grants` and `testing`: what it printed, and how it ended.
fn browse(
    test: &str,
    what: &str,
    name: &str,
    page: &str,
    grants: impl FnOnce(&Path) -> Grants,
    testing: Testing,
) -> (Vec<String>, Ended) {
    let root = scratch(&format!("browser-{test}"));
    std::fs::write(root.join(name), page).expect("the page");
    browse_in(&root, what, name, "scratch", grants, testing)
}

/// [`browse`], in `root`, the page already there, served from the root named `served`.
fn browse_in(
    root: &Path,
    what: &str,
    name: &str,
    served: &str,
    grants: impl FnOnce(&Path) -> Grants,
    testing: Testing,
) -> (Vec<String>, Ended) {
    // Built once, for the tests at once.
    static BUILD: std::sync::OnceLock<wrela_host::tool::ToolBuild> = std::sync::OnceLock::new();
    let build = BUILD.get_or_init(|| tool_build("browser-tool", BROWSER_TOOL));
    let (lines, mut options) = printer(|_| {});
    options.caps = Caps::open(&grants(root), |_| None).expect("granted");
    options.args = vec![what.to_string(), name.to_string(), served.to_string()];
    options.timeout = Some(Duration::from_secs(60));
    options.testing = testing;
    let ended = run(build, options);
    let printed = lines.lock().expect("the lines").clone();
    (printed, ended)
}

/// The processes whose command line names `path`.
fn processes_naming(path: &Path) -> Vec<String> {
    let out = std::process::Command::new("ps").args(["-Ao", "pid,command"]).output().expect("ps");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let path = path.display().to_string();
    text.lines().filter(|l| l.contains(&path)).map(str::to_string).collect()
}

#[test]
#[ignore = "needs the runtime"]
fn a_tool_acts_on_a_page_and_reads_what_it_shows() {
    let (printed, ended) = browse("act", "act", "form.html", FORM, grants, Testing::default());
    assert_eq!(ended.end, End::Exit(0), "{printed:?}");
    let want = [
        "opened",
        "title typed aBé",
        "items 2 one two",
        "count 1",
        "href #next",
        "active button ",
        "page true",
        "capture 320x240 png true",
    ];
    assert_eq!(&printed[..want.len()], want, "{printed:?}");
    assert!(printed.contains(&"console ready".to_string()), "{printed:?}");
    assert_eq!(printed.last().map(String::as_str), Some("closed"), "{printed:?}");
    assert_eq!(ended.blobs_released.0, 1, "the capture's blob, which the tool kept");
}

/// No connection a page or its worker makes reaches what the tool isn't granted: neither plain
/// HTTP nor a WebSocket, secure or not, nor WebTransport, to another port of this machine or of
/// a granted host. Every one is refused, and no listener there is reached.
#[test]
#[ignore = "needs the runtime"]
fn a_page_reaches_only_its_tools_servers() {
    let listen = || {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("listen");
        l.set_nonblocking(true).expect("nonblocking");
        let port = l.local_addr().expect("addr").port();
        (l, port)
    };
    let datagrams = || {
        let u = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
        u.set_nonblocking(true).expect("nonblocking");
        let port = u.local_addr().expect("addr").port();
        (u, port)
    };
    let ((other, op), (other2, op2)) = (listen(), listen());
    let ((udp, up), (udp2, up2)) = (datagrams(), datagrams());
    let page = REFUSING
        .replace("OTHER2", &op2.to_string())
        .replace("OTHER", &op.to_string())
        .replace("DATAGRAMS2", &up2.to_string())
        .replace("DATAGRAMS", &up.to_string());
    let with_localhost = |root: &Path| grants(root).net("localhost");
    let (printed, ended) =
        browse("refused", "refused", "refusing.html", &page, with_localhost, Testing::default());
    assert_eq!(ended.end, End::Exit(0), "{printed:?}");
    let refused = ["refused"; 8].join(" ");
    let title = format!("title tried {refused} | {refused}");
    assert!(printed.contains(&title), "{printed:?}");
    for (what, l) in
        [("this machine's other port", &other), ("the granted host's other port", &other2)]
    {
        assert!(l.accept().is_err(), "nothing reached {what}");
    }
    let mut buf = [0u8; 64];
    assert!(udp.recv(&mut buf).is_err() && udp2.recv(&mut buf).is_err(), "no datagram came");
    let said = |what: &str| printed.iter().any(|l| l.contains(what));
    assert!(said(&format!("refused GET http://127.0.0.1:{op}/x")), "{printed:?}");
    assert!(said(&format!("refused a connection to 127.0.0.1:{op}")), "{printed:?}");
    assert!(said(&format!("refused a connection to localhost:{op2}")), "{printed:?}");
    assert!(said("refused a connection to example.com:443"), "{printed:?}");
}

/// A close is taken as the host takes it: with four browsers waiting 600 s each, on every
/// worker that waits, a close of one ends its wait at once, and so finds a worker, and both end
/// within the cleanup grace.
#[test]
#[ignore = "needs the runtime"]
fn a_close_queued_behind_every_worker_still_ends_its_browsers_wait() {
    let moments = Arc::new(std::sync::Mutex::new(Vec::new()));
    let kept = moments.clone();
    let signal = Signal(Arc::new(move |m| kept.lock().expect("moments").push((m, Instant::now()))));
    let testing = Testing { signal: Some(signal), ..Testing::default() };
    let (printed, ended) = browse("busy", "busy", "never.html", NEVER, grants, testing);
    assert_eq!(ended.end, End::Exit(0), "{printed:?}");
    let seen = moments.lock().expect("moments").clone();
    let closing = seen.iter().find(|(m, _)| *m == "closing").map(|(_, t)| *t).expect("taken");
    let runs = seen.iter().filter(|(m, t)| *m == "run" && *t < closing).count();
    assert_eq!(runs, 4, "every long worker waited as the close came: {seen:?}");
    assert!(printed[1].starts_with("run: ") && printed[1].contains("closed"), "{printed:?}");
    assert_eq!(printed[2], "close: closed");
    let grace = Testing::default().grace;
    assert!(closing.elapsed() < grace, "both ended within the grace: {:?}", closing.elapsed());
}

/// A result past the most that's read of one is an error that says so, never its first part:
/// "ok", a MiB of spaces, then "failed" isn't "ok".
#[test]
#[ignore = "needs the runtime"]
fn a_result_past_its_limit_fails_rather_than_read_in_part() {
    let (printed, ended) = browse("huge", "huge", "huge.html", HUGE, grants, Testing::default());
    assert_eq!(ended.end, End::Exit(0), "{printed:?}");
    assert!(
        printed[1].starts_with("run: ") && printed[1].contains("more than the 1048576"),
        "{printed:?}"
    );
    assert!(
        printed[2].starts_with("results: ") && printed[2].contains("more than the 1048576"),
        "{printed:?}"
    );
}

/// WebRTC sends no UDP around the proxy: a page gathering ICE candidates against a STUN and a
/// TURN server on a listening UDP port of this machine sends it no datagram.
#[test]
#[ignore = "needs the runtime"]
fn webrtc_sends_no_datagram_past_the_proxy() {
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    udp.set_nonblocking(true).expect("nonblocking");
    let port = udp.local_addr().expect("addr").port();
    let page = GATHERING.replace("DATAGRAMS", &port.to_string());
    let (printed, ended) =
        browse("gather", "gather", "gather.html", &page, grants, Testing::default());
    assert_eq!(ended.end, End::Exit(0), "{printed:?}");
    assert!(printed.iter().any(|l| l.starts_with("title gathered")), "{printed:?}");
    let mut buf = [0u8; 1500];
    assert!(udp.recv(&mut buf).is_err(), "a datagram reached the STUN server's port: {printed:?}");
}

/// A page's result is its own browser's: of two browsers on one server, the one whose page puts
/// its result sees it, and the other, whose page puts none, has none and waits on.
#[test]
#[ignore = "needs the runtime"]
fn a_result_is_its_own_browsers() {
    let root = scratch("browser-two");
    std::fs::write(root.join("done.html"), DONE).expect("the page");
    std::fs::write(root.join("never.html"), NEVER).expect("the page");
    let (printed, ended) =
        browse_in(&root, "two", "never.html", "scratch", grants, Testing::default());
    assert_eq!(ended.end, End::Exit(0), "{printed:?}");
    assert_eq!(printed[1], "the second's page is done", "{printed:?}");
    assert_eq!(printed[2], "the first has no result", "{printed:?}");
    assert!(printed[3].contains("never had the page's results/DONE"), "{printed:?}");
}

/// No observation is cut short: all of 4,097 elements' texts, and a clipboard of 20,000 bytes.
#[test]
#[ignore = "needs the runtime"]
fn observations_are_whole() {
    let (printed, ended) =
        browse("whole", "whole", "whole.html", WHOLE, grants, Testing::default());
    assert_eq!(ended.end, End::Exit(0), "{printed:?}");
    let want = ["opened", "items 4097, the last item 4096", "clipboard 20000 bytes", "closed"];
    assert_eq!(printed, want);
}

/// A close while a script waits 600 s for a result ends both at once: the wait fails, saying the
/// browser was closed, and the close succeeds, within the cleanup grace.
#[test]
#[ignore = "needs the runtime"]
fn a_close_during_a_long_wait_ends_both_at_once() {
    let moments = Arc::new(std::sync::Mutex::new(Vec::new()));
    let kept = moments.clone();
    let signal = Signal(Arc::new(move |m| kept.lock().expect("moments").push((m, Instant::now()))));
    let testing = Testing { signal: Some(signal), ..Testing::default() };
    let (printed, ended) = browse("closing", "closing", "never.html", NEVER, grants, testing);
    assert_eq!(ended.end, End::Exit(0), "{printed:?}");
    let at = |name: &str| {
        moments.lock().expect("moments").iter().find(|(m, _)| *m == name).map(|(_, t)| *t)
    };
    let (run, close) = (at("run").expect("the wait ran"), at("close").expect("the close ran"));
    assert!(run < close, "the close came while the wait waited");
    assert_eq!(printed[0], "opened");
    assert!(printed[1].starts_with("run: ") && printed[1].contains("closed"), "{printed:?}");
    assert_eq!(printed[2], "close: closed");
    let grace = Testing::default().grace;
    assert!(close.elapsed() < grace, "both ended within the grace: {:?}", close.elapsed());
}

/// A page's result is seen once its server has kept it: a wait for one whose write the server
/// holds a second ends only after it's on disk, and one the server refuses (its root is read
/// only) is never seen.
#[test]
#[ignore = "needs the runtime"]
fn a_result_is_seen_only_once_its_server_kept_it() {
    let held = Testing {
        hold: Some(("results/DONE".into(), Duration::from_secs(1))),
        ..Testing::default()
    };
    let (printed, ended) = browse("delayed", "delayed", "done.html", DONE, grants, held);
    assert_eq!(ended.end, End::Exit(0), "{printed:?}");
    assert_eq!(printed, ["opened", "kept 2 bytes", "closed"]);
    let read_only = |root: &Path| {
        Grants { browser: true, ..Grants::default() }
            .root("scratch", root.join("scratch-unused"), true)
            .root("site", root, false)
    };
    let root = scratch("browser-rejected");
    std::fs::create_dir_all(root.join("scratch-unused")).expect("a write root");
    std::fs::write(root.join("done.html"), DONE).expect("the page");
    let (printed, ended) =
        browse_in(&root, "rejected", "done.html", "site", read_only, Testing::default());
    assert_eq!(ended.end, End::Exit(0), "{printed:?}");
    assert!(printed[1].contains("never had the page's results/DONE"), "{printed:?}");
    assert_eq!(printed[2], "kept no DONE");
}

#[test]
#[ignore = "needs the runtime"]
fn the_clipboard_is_read_only_once_it_is_granted() {
    let (printed, ended) =
        browse("clipboard", "clipboard", "clipboard.html", CLIPBOARD, grants, Testing::default());
    assert_eq!(ended.end, End::Exit(0), "{printed:?}");
    let want = ["opened", "clipboard before: refused", "clipboard after: copied!", "closed"];
    assert_eq!(printed, want);
}

#[test]
#[ignore = "needs the runtime"]
fn a_pages_result_is_seen_and_written_to_its_root() {
    let (printed, ended) = browse("done", "done", "done.html", DONE, grants, Testing::default());
    assert_eq!(ended.end, End::Exit(0), "{printed:?}");
    assert_eq!(printed, ["opened", "done ok", "closed"]);
}

#[test]
#[ignore = "needs the runtime"]
fn a_page_that_never_reports_fails_at_its_wait_and_its_deadline() {
    let began = Instant::now();
    let (printed, ended) =
        browse("never", "never", "never.html", NEVER, grants, Testing::default());
    assert_eq!(ended.end, End::Exit(0), "{printed:?}");
    let run = printed.iter().find(|l| l.starts_with("run: ")).expect("the wait failed");
    assert!(run.contains("never had the page's results/DONE within 0.3 s"), "{run}");
    let deadline = printed.iter().find(|l| l.starts_with("deadline: ")).expect("the deadline");
    assert!(deadline.contains("past its deadline of 0.5 s"), "{deadline}");
    assert!(deadline.contains("by the script's deadline"), "{deadline}");
    assert!(began.elapsed() < Duration::from_secs(30), "{:?}", began.elapsed());
}

#[test]
#[ignore = "needs the runtime"]
fn a_browser_needs_its_grant() {
    let ungranted = |root: &Path| Grants::default().root("scratch", root, true);
    let (printed, ended) =
        browse("ungranted", "act", "form.html", FORM, ungranted, Testing::default());
    assert_eq!(ended.end, End::Exit(1), "{printed:?}");
    assert_eq!(printed, ["open: the browser isn't granted: `--browser` grants it"]);
}

/// A tool that traps with a browser open ends with no Chrome process left and its profile gone:
/// while it runs, Chrome's processes name the run's own directory, where its profile is.
#[test]
#[ignore = "needs the runtime"]
fn a_trap_with_a_browser_open_leaves_no_chrome_and_no_profile() {
    let work: PathBuf = scratch("browser-trap-work").join("run");
    // Seen at the moment the browser has opened, before its answer reaches the tool: the host
    // waits for this.
    let (seen, saw) = mpsc::channel();
    let (watched, seen) = (work.clone(), std::sync::Mutex::new(seen));
    let signal = Signal(Arc::new(move |moment| {
        if moment == "browser" {
            let _ = seen.lock().expect("the channel").send(processes_naming(&watched));
        }
    }));
    let testing =
        Testing { work_dir: Some(work.clone()), signal: Some(signal), ..Testing::default() };
    let (printed, ended) = browse("trap", "trap", "never.html", NEVER, grants, testing);
    let End::Trap(why) = &ended.end else { panic!("a trap: {ended:?} {printed:?}") };
    assert!(why.contains("a trap with a browser open"), "{why}");
    let running = saw.recv().expect("what ran");
    assert!(!running.is_empty(), "Chrome ran with its profile in the run's directory");
    assert!(processes_naming(&work).is_empty(), "no Chrome is left: {:?}", processes_naming(&work));
    assert!(!work.exists(), "the run's directory, the profile in it, is gone");
}
