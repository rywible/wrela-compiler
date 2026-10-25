//! The DevTools protocol over Chrome's pipe (`--remote-debugging-pipe`): Chrome reads messages
//! on its fd 3 and writes them on its fd 4, each a JSON text ended by a NUL byte. One pipe pair
//! carries the browser, its pages' and their workers' sessions (`Target.attachToTarget` with
//! `flatten`), so each message names its session.
//!
//! **Chrome ends with the pipe.** When the pipe's other end closes, because this connection
//! closed it or because the process that held it died, even by `SIGKILL`, Chrome shuts down.
//!
//! **A thread writes and a thread reads.** Callers queue what they send, so none ever holds the
//! pipe; the reader gives an answer to the caller waiting for its `id`, and an event to the
//! channel of its session, and routes a session that a routed session's auto-attach makes to the
//! same channel before its first event can arrive. A caller waits at most until its deadline.
//! When the pipe closes or fails, every waiting caller is told why, and every channel ends.

use crate::lock;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// The largest message Chrome may send: a screenshot's base64 at 16384 pixels each way.
const MOST_MESSAGE: usize = 1536 << 20;

/// Other replies and page events must not use a screenshot's allowance. A page can make a
/// console event arbitrarily large, without a screenshot request or blob budget.
const MOST_ORDINARY_MESSAGE: usize = 1 << 20;

/// A page can produce events faster than its tab handles them. Bound the route before console
/// events from that page can grow the driver's memory without limit.
pub(crate) const MOST_EVENTS: usize = 64;
const _: () = assert!(MOST_EVENTS * MOST_ORDINARY_MESSAGE <= 64 << 20);

/// An event of a session: its method and parameters.
#[derive(Clone, Debug)]
pub struct Event {
    pub session: String,
    pub method: String,
    pub params: Value,
}

struct Pending {
    reply: mpsc::Sender<Result<Value, String>>,
    screenshot: bool,
}

/// What the threads and the callers share.
#[derive(Default)]
struct Shared {
    pending: Mutex<HashMap<u64, Pending>>,
    /// Each session's channel, with the tag of the route it came from.
    routes: Mutex<HashMap<String, (u64, mpsc::SyncSender<Event>)>>,
    /// Why the connection ended, once it has.
    ended: Mutex<Option<String>>,
}

impl Shared {
    /// Ends the connection for `why`: each waiting caller hears it, and each channel ends.
    fn end(&self, why: String) {
        let mut ended = lock(&self.ended);
        if ended.is_none() {
            *ended = Some(why.clone());
        }
        drop(ended);
        for (_, pending) in lock(&self.pending).drain() {
            let _ = pending.reply.send(Err(why.clone()));
        }
        lock(&self.routes).clear();
    }
}

/// A connection to a browser's DevTools.
pub struct Connection {
    shared: Arc<Shared>,
    out: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    next: AtomicU64,
    tags: AtomicU64,
    writer: Mutex<Option<std::thread::JoinHandle<()>>>,
    reader: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Connection {
    /// The connection over the pipe's two ends: what writes to Chrome, and what reads from it.
    pub fn start(
        to: impl Write + Send + 'static,
        from: impl Read + Send + 'static,
    ) -> Result<Connection, String> {
        let shared = Arc::new(Shared::default());
        let (out, queued) = mpsc::channel::<Vec<u8>>();
        let s = shared.clone();
        let writer = std::thread::Builder::new()
            .name("wrela-chrome-write".into())
            .spawn(move || write_all(to, &queued, &s))
            .map_err(|e| format!("can't start the DevTools writer: {e}"))?;
        let s = shared.clone();
        let reader = std::thread::Builder::new()
            .name("wrela-chrome-read".into())
            .spawn(move || read_all(from, &s));
        let reader = match reader {
            Ok(t) => t,
            Err(e) => {
                // The writer ends with its queue, and the pipe with it.
                drop(out);
                let _ = writer.join();
                return Err(format!("can't start the DevTools reader: {e}"));
            }
        };
        Ok(Connection {
            shared,
            out: Mutex::new(Some(out)),
            next: AtomicU64::new(1),
            tags: AtomicU64::new(1),
            writer: Mutex::new(Some(writer)),
            reader: Mutex::new(Some(reader)),
        })
    }

    /// Why the connection ended, if it has.
    pub fn ended(&self) -> Option<String> {
        lock(&self.shared.ended).clone()
    }

    /// Routes session `session`'s events (`""`: the browser's own) to `to`, and those of each
    /// session it attaches to: the route's tag, which [`Connection::unroute`] takes.
    pub fn route(&self, session: &str, to: mpsc::SyncSender<Event>) -> u64 {
        let tag = self.tags.fetch_add(1, Ordering::Relaxed);
        if self.ended().is_none() {
            lock(&self.shared.routes).insert(session.to_string(), (tag, to));
        }
        tag
    }

    /// Stops routing the sessions of route `tag`: its channel ends.
    pub fn unroute(&self, tag: u64) {
        lock(&self.shared.routes).retain(|_, (t, _)| *t != tag);
    }

    /// Stops routing session `session` (it has detached).
    pub fn unroute_session(&self, session: &str) {
        lock(&self.shared.routes).remove(session);
    }

    /// Calls `method` with `params` in `session` (`None`: the browser's), waiting for its answer
    /// until `deadline`: its result, or why there's none. Its reply is at most
    /// [`MOST_ORDINARY_MESSAGE`] bytes.
    pub fn call(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, String> {
        self.request(session, method, params, deadline, false)
    }

    /// Calls `Page.captureScreenshot` with `params` in `session`, as [`Connection::call`] does:
    /// its reply, a `data` string, may take up to [`MOST_MESSAGE`] bytes.
    pub fn call_screenshot(
        &self,
        session: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, String> {
        self.request(Some(session), "Page.captureScreenshot", params, deadline, true)
    }

    fn request(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
        deadline: Instant,
        screenshot: bool,
    ) -> Result<Value, String> {
        if let Some(why) = self.ended() {
            return Err(format!("{method}: {why}"));
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut message = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            message["sessionId"] = Value::String(s.to_string());
        }
        let (reply, answer) = mpsc::channel();
        lock(&self.shared.pending).insert(id, Pending { reply, screenshot });
        // The connection may have ended between the check and the insert: it drained `pending`.
        if let Some(why) = self.ended() {
            lock(&self.shared.pending).remove(&id);
            return Err(format!("{method}: {why}"));
        }
        let mut bytes = message.to_string().into_bytes();
        bytes.push(0);
        let sent = lock(&self.out).as_ref().is_some_and(|o| o.send(bytes).is_ok());
        if !sent {
            lock(&self.shared.pending).remove(&id);
            return Err(format!("{method}: the DevTools connection has closed"));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match answer.recv_timeout(left) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(why)) => Err(format!("{method}: {why}")),
            Err(_) => {
                lock(&self.shared.pending).remove(&id);
                Err(format!("{method}: no answer by its deadline"))
            }
        }
    }

    /// Closes the connection: every waiting caller fails at once, the pipe to Chrome closes (and
    /// Chrome shuts down), and the writer's thread is joined, waiting at most `grace`: whether
    /// it ended.
    pub fn close(&self, grace: Duration) -> bool {
        self.cut();
        join(&self.writer, grace)
    }

    /// Closes the connection without waiting for anything: every waiting caller fails at once,
    /// and the writer's queue ends, so it closes the pipe as it returns.
    pub fn cut(&self) {
        self.shared.end("the DevTools connection was closed".into());
        lock(&self.out).take();
    }

    /// Waits at most `grace` for the reader's thread, which ends when Chrome's end of the pipe
    /// closes (it has exited): whether it ended.
    pub fn join_reader(&self, grace: Duration) -> bool {
        join(&self.reader, grace)
    }
}

/// Joins the thread in `t`, if it ends within `grace`.
fn join(t: &Mutex<Option<std::thread::JoinHandle<()>>>, grace: Duration) -> bool {
    let mut held = lock(t);
    let Some(thread) = held.as_ref() else { return true };
    if !crate::finished_within(thread, grace) {
        return false;
    }
    if let Some(thread) = held.take() {
        let _ = thread.join();
    }
    true
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close(Duration::from_secs(1));
    }
}

/// The writer's thread: each queued message, until the queue ends (the connection closed) or a
/// write fails. Its end of the pipe closes as it returns.
fn write_all(mut to: impl Write, queued: &mpsc::Receiver<Vec<u8>>, s: &Shared) {
    while let Ok(bytes) = queued.recv() {
        if let Err(e) = to.write_all(&bytes).and_then(|()| to.flush()) {
            s.end(format!("DevTools: {e}"));
            return;
        }
    }
}

/// The reader's thread: each message, until Chrome's end closes.
fn read_all(from: impl Read, s: &Shared) {
    let mut from = BufReader::with_capacity(1 << 20, from);
    let why = loop {
        let mut message = Vec::new();
        let read = (&mut from).take(MOST_ORDINARY_MESSAGE as u64 + 1).read_until(0, &mut message);
        match read {
            Ok(0) => break "Chrome closed its DevTools pipe".to_string(),
            Ok(_) => {
                let ended = message.last() == Some(&0);
                let size = message.len() - usize::from(ended);
                if size > MOST_ORDINARY_MESSAGE {
                    if !screenshot_reply(&message, s) {
                        break format!(
                            "DevTools sent a non-screenshot message of more than {MOST_ORDINARY_MESSAGE} bytes"
                        );
                    }
                    if !ended {
                        let left = MOST_MESSAGE + 1 - message.len();
                        match (&mut from).take(left as u64).read_until(0, &mut message) {
                            Ok(_) if message.last() == Some(&0) => {}
                            Ok(_) if message.len() > MOST_MESSAGE => {
                                break format!(
                                    "DevTools sent a message of more than {MOST_MESSAGE} bytes"
                                );
                            }
                            Ok(_) => break "Chrome closed its DevTools pipe".to_string(),
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(e) => break format!("DevTools: {e}"),
                        }
                    }
                }
                if message.last() != Some(&0) {
                    break "Chrome closed its DevTools pipe".to_string();
                }
                message.pop();
                dispatch(s, &message);
                if let Some(why) = lock(&s.ended).clone() {
                    break why;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => break format!("DevTools: {e}"),
        }
    };
    s.end(why);
}

/// A screenshot reply has a top-level `result` whose `data` is a string. Chrome may put its
/// top-level fields in any order: if `id` is after `data`, the pending screenshot call is checked
/// when the complete reply is dispatched. Events have `method`, not this `result` shape.
fn screenshot_reply(prefix: &[u8], s: &Shared) -> bool {
    let mut i = 0;
    skip_ws(prefix, &mut i);
    if prefix.get(i) != Some(&b'{') {
        return false;
    }
    i += 1;
    let mut id = None;
    loop {
        skip_ws(prefix, &mut i);
        let Some(key) = json_string(prefix, &mut i) else { return false };
        skip_ws(prefix, &mut i);
        if prefix.get(i) != Some(&b':') {
            return false;
        }
        i += 1;
        skip_ws(prefix, &mut i);
        if key == b"id" {
            let start = i;
            while prefix.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            let Ok(n) = std::str::from_utf8(&prefix[start..i]).unwrap_or("").parse::<u64>() else {
                return false;
            };
            id = Some(n);
        } else if key == b"sessionId" {
            if json_string(prefix, &mut i).is_none() {
                return false;
            }
        } else if key == b"result" {
            if prefix.get(i) != Some(&b'{') {
                return false;
            }
            i += 1;
            skip_ws(prefix, &mut i);
            if json_string(prefix, &mut i) != Some(b"data".as_slice()) {
                return false;
            }
            skip_ws(prefix, &mut i);
            if prefix.get(i) != Some(&b':') {
                return false;
            }
            i += 1;
            skip_ws(prefix, &mut i);
            if prefix.get(i) != Some(&b'"') {
                return false;
            }
            let pending = lock(&s.pending);
            return match id {
                Some(id) => pending.get(&id).is_some_and(|p| p.screenshot),
                None => pending.values().any(|p| p.screenshot),
            };
        } else {
            return false;
        }
        skip_ws(prefix, &mut i);
        if prefix.get(i) != Some(&b',') {
            return false;
        }
        i += 1;
    }
}

fn skip_ws(bytes: &[u8], i: &mut usize) {
    while bytes.get(*i).is_some_and(u8::is_ascii_whitespace) {
        *i += 1;
    }
}

/// A JSON string's raw bytes, with escaped quotes skipped. CDP's field names are ASCII.
fn json_string<'a>(bytes: &'a [u8], i: &mut usize) -> Option<&'a [u8]> {
    if bytes.get(*i) != Some(&b'"') {
        return None;
    }
    *i += 1;
    let start = *i;
    while let Some(&b) = bytes.get(*i) {
        match b {
            b'"' => {
                let out = &bytes[start..*i];
                *i += 1;
                return Some(out);
            }
            b'\\' => *i += 2,
            _ => *i += 1,
        }
    }
    None
}

/// Gives a message to the caller waiting for its answer, or to its session's channel.
fn dispatch(s: &Shared, bytes: &[u8]) {
    let Ok(mut message) = serde_json::from_slice::<Value>(bytes) else { return };
    if let Some(id) = message.get("id").and_then(Value::as_u64) {
        let Some(pending) = lock(&s.pending).remove(&id) else { return };
        let answer = match message.get("error") {
            Some(e) => {
                Err(e.get("message").and_then(Value::as_str).unwrap_or("an error").to_string())
            }
            None => Ok(message.get_mut("result").map(Value::take).unwrap_or(Value::Null)),
        };
        let _ = pending.reply.send(answer);
        return;
    }
    let Some(method) = message.get("method").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let session = message.get("sessionId").and_then(Value::as_str).unwrap_or("").to_string();
    let params = message.get_mut("params").map(Value::take).unwrap_or(Value::Null);
    let mut routes = lock(&s.routes);
    let Some((tag, to)) = routes.get(&session).cloned() else { return };
    // A session this one attached to is routed with it before any of its own events come.
    if method == "Target.attachedToTarget"
        && let Some(child) = params.get("sessionId").and_then(Value::as_str)
    {
        routes.insert(child.to_string(), (tag, to.clone()));
    }
    drop(routes);
    let key = session.clone();
    match to.try_send(Event { session, method, params }) {
        Ok(()) => {}
        Err(mpsc::TrySendError::Full(_)) => {
            s.end(format!("DevTools queued more than {MOST_EVENTS} events for a tab"));
        }
        Err(mpsc::TrySendError::Disconnected(_)) => {
            // Nobody reads this session's events any more.
            lock(&s.routes).remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake Chrome on the other ends of two pipes: for each message it reads, it sends an
    /// event, then what `answer` gives for it, with its `id`. What it read, once its input ends.
    fn fake(
        answer: impl Fn(&Value) -> Option<Value> + Send + 'static,
    ) -> (Connection, std::thread::JoinHandle<Vec<String>>) {
        let (chrome_reads, to) = std::io::pipe().expect("a pipe");
        let (from, mut chrome_writes) = std::io::pipe().expect("a pipe");
        let chrome = std::thread::spawn(move || {
            let mut seen = Vec::new();
            let mut input = BufReader::new(chrome_reads);
            loop {
                let mut m = Vec::new();
                if input.read_until(0, &mut m).unwrap_or(0) == 0 {
                    break;
                }
                m.pop();
                let m: Value = serde_json::from_slice(&m).expect("json");
                seen.push(m["method"].as_str().unwrap_or("").to_string());
                let event =
                    json!({"method": "Page.event", "sessionId": "s", "params": m["params"]});
                let mut out = event.to_string().into_bytes();
                out.push(0);
                if let Some(mut a) = answer(&m) {
                    a["id"] = m["id"].clone();
                    out.extend(a.to_string().into_bytes());
                    out.push(0);
                }
                if chrome_writes.write_all(&out).is_err() {
                    break;
                }
            }
            seen
        });
        (Connection::start(to, from).expect("a connection"), chrome)
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn calls_are_answered_in_order_and_events_reach_their_session() {
        let (conn, chrome) = fake(|m| Some(json!({"result": {"ok": m["method"]}})));
        let (to, events) = mpsc::sync_channel(MOST_EVENTS);
        conn.route("s", to);
        let a = conn.call(Some("s"), "Input.insertText", json!({"text": "か"}), soon());
        assert_eq!(a.expect("answered"), json!({"ok": "Input.insertText"}));
        let b = conn.call(None, "Browser.getVersion", json!({}), soon()).expect("answered");
        assert_eq!(b, json!({"ok": "Browser.getVersion"}));
        let e = events.recv_timeout(Duration::from_secs(5)).expect("an event");
        assert_eq!((e.session.as_str(), e.method.as_str()), ("s", "Page.event"));
        assert!(conn.close(Duration::from_secs(5)), "its writer ends, and the pipe with it");
        assert_eq!(chrome.join().expect("the fake"), ["Input.insertText", "Browser.getVersion"]);
        assert!(conn.join_reader(Duration::from_secs(5)), "Chrome's end closed: the reader ends");
    }

    #[test]
    fn nested_reply_and_event_values_survive_dispatch() {
        let (conn, chrome) = fake(|m| Some(json!({"result": m["params"]})));
        let (to, events) = mpsc::sync_channel(MOST_EVENTS);
        conn.route("s", to);
        let params = json!({
            "data": "large reply ".repeat(16_384),
            "nested": [null, {"values": [true, 7, "か"]}]
        });
        let reply = conn.call(Some("s"), "Runtime.evaluate", params.clone(), soon());
        assert_eq!(reply.expect("reply"), params);
        let event = events.recv_timeout(Duration::from_secs(5)).expect("event");
        assert_eq!(event.params, params);
        assert_eq!((event.session.as_str(), event.method.as_str()), ("s", "Page.event"));
        assert!(conn.close(Duration::from_secs(5)));
        chrome.join().expect("fake Chrome");
        assert!(conn.join_reader(Duration::from_secs(5)));
    }

    #[test]
    fn an_error_says_why_and_a_closed_pipe_fails_every_caller() {
        let (conn, chrome) = fake(|m| {
            (m["method"] != "Page.navigate").then(|| json!({"error": {"message": "no such key"}}))
        });
        let e = conn.call(None, "Input.dispatchKeyEvent", json!({}), soon()).expect_err("an error");
        assert_eq!(e, "Input.dispatchKeyEvent: no such key");
        // A call waiting when the pipe closes fails at once, not at its deadline.
        let began = Instant::now();
        std::thread::scope(|sc| {
            let waiting = sc.spawn(|| {
                conn.call(
                    None,
                    "Page.navigate",
                    json!({}),
                    Instant::now() + Duration::from_secs(60),
                )
            });
            while lock(&conn.shared.pending).is_empty() {
                std::thread::yield_now();
            }
            conn.close(Duration::from_secs(5));
            let e = waiting.join().expect("the caller").expect_err("closed");
            assert!(e.contains("closed"), "{e}");
        });
        assert!(began.elapsed() < Duration::from_secs(5));
        chrome.join().expect("the fake");
        let e = conn.call(None, "Page.navigate", json!({}), soon()).expect_err("closed");
        assert!(e.contains("closed"), "{e}");
    }

    #[test]
    fn a_call_nobody_answers_fails_at_its_deadline_and_chromes_exit_ends_the_reader() {
        let (conn, chrome) = fake(|_| None);
        let began = Instant::now();
        let e = conn
            .call(None, "Runtime.evaluate", json!({}), Instant::now() + Duration::from_millis(200))
            .expect_err("no answer");
        assert!(e.contains("deadline"), "{e}");
        assert!(began.elapsed() < Duration::from_secs(2), "{:?}", began.elapsed());
        conn.close(Duration::from_secs(5));
        chrome.join().expect("the fake");
        assert!(conn.join_reader(Duration::from_secs(5)));
        assert_eq!(conn.ended().as_deref(), Some("the DevTools connection was closed"));
    }

    #[test]
    fn a_page_cannot_queue_unbounded_events() {
        let shared = Shared::default();
        let (to, events) = mpsc::sync_channel(MOST_EVENTS);
        lock(&shared.routes).insert("s".into(), (1, to));
        let event = br#"{"sessionId":"s","method":"Runtime.consoleAPICalled","params":{}}"#;
        for _ in 0..MOST_EVENTS {
            dispatch(&shared, event);
        }
        assert!(lock(&shared.ended).is_none());
        dispatch(&shared, event);
        assert!(lock(&shared.ended).as_deref().is_some_and(|e| e.contains("queued more than")));
        assert_eq!(events.try_iter().count(), MOST_EVENTS);
    }

    #[test]
    fn a_large_page_event_cannot_use_the_screenshot_message_limit() {
        let shared = Shared::default();
        let (reply, _answer) = mpsc::channel();
        lock(&shared.pending).insert(7, Pending { reply, screenshot: true });
        let mut event =
            br#"{"method":"Runtime.consoleAPICalled","params":{"result":{"data":""#.to_vec();
        event.resize(MOST_ORDINARY_MESSAGE + 1, b'x');
        read_all(std::io::Cursor::new(event), &shared);
        assert!(
            lock(&shared.ended)
                .as_deref()
                .is_some_and(|why| why.contains("non-screenshot message"))
        );
    }

    #[test]
    fn a_screenshot_reply_may_exceed_the_ordinary_message_limit() {
        let shared = Shared::default();
        let (reply, answer) = mpsc::channel();
        lock(&shared.pending).insert(7, Pending { reply, screenshot: true });
        let mut message = b"{\"id\":7,\"result\":{\"data\":\"".to_vec();
        message.resize(message.len() + MOST_ORDINARY_MESSAGE, b'A');
        message.extend_from_slice(b"\"}}\0");
        read_all(std::io::Cursor::new(message), &shared);
        let got = answer.try_recv().expect("the screenshot reply").expect("no CDP error");
        assert_eq!(got["data"].as_str().unwrap().len(), MOST_ORDINARY_MESSAGE);
    }

    #[test]
    fn a_screenshot_reply_may_put_its_id_after_its_data() {
        let shared = Shared::default();
        let (reply, answer) = mpsc::channel();
        lock(&shared.pending).insert(7, Pending { reply, screenshot: true });
        let mut message = b"{\"sessionId\":\"s\",\"result\":{\"data\":\"".to_vec();
        message.resize(message.len() + MOST_ORDINARY_MESSAGE, b'A');
        message.extend_from_slice(b"\"},\"id\":7}\0");
        read_all(std::io::Cursor::new(message), &shared);
        let got = answer.try_recv().expect("the screenshot reply").expect("no CDP error");
        assert_eq!(got["data"].as_str().unwrap().len(), MOST_ORDINARY_MESSAGE);
    }
}
