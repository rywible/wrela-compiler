//! A browser script (`std::tool::browser::Script`, language.md §15.5): typed steps that act on a
//! page as a person does, configure it, and wait for typed conditions; and typed observations of
//! what the page shows. Nothing here is JavaScript: the driver's one function in the page
//! ([`OBSERVE`]) reads what an observation names, and every comparison a wait makes is here, in
//! Rust, against what that function read.
//!
//! **The format** is std's: a JSON array of steps, each an object with a `step`:
//! - `{"step": "wait", "until": UNTIL, "timeout": s}`: until the condition holds, at most `s`
//!   seconds;
//! - `{"step": "click", "on": ELEMENT, "timeout": s}`: once an element matches (at most `s`
//!   seconds), a trusted click at its centre, scrolled into view; `{"step": "click_at", "x",
//!   "y"}`: a click at a point of the viewport;
//! - `{"step": "press", "key": n, "shift", "ctrl", "alt", "meta"}`: a key pressed and released
//!   (`n` is `std::input::Key`'s number); `{"step": "insert", "text"}`: text an input method
//!   commits; `{"step": "compose", "text", "start", "end"}`: an input method's composition, its
//!   selection from `start` to `end`;
//! - `{"step": "focus", "on": ELEMENT, "timeout": s}`; `{"step": "grant", "permissions":
//!   ["clipboard"]}`; `{"step": "emulate_focus", "on": bool}`; `{"step": "size", "width",
//!   "height"}`.
//!
//! An ELEMENT is `{"css": selector}`, with `"text"` for the first of them whose text is exactly
//! that. An UNTIL is one of `{"title": MATCH}`, `{"exists": ELEMENT}`, `{"count": ELEMENT, "n"}`,
//! `{"texts": selector, "are": [...]}` (each match's text, in document order), `{"attribute":
//! ELEMENT, "name", "is": MATCH}` (the first match's), `{"active": tag}`, `{"location": MATCH}`,
//! `{"hash": MATCH}`, `{"clipboard": MATCH}`, `{"printed": line}` (the program printed it),
//! `{"done": true}` (the page put `results/DONE`), or `{"all": [UNTIL, ...]}`. A MATCH is
//! `{"is" | "starts" | "ends" | "contains": text}`.

use crate::Tab;
use crate::keys::Mods;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// The driver's one function in the page: what an observation reads, by its kind. Elements
/// match by selector, then by exact text.
pub(crate) const OBSERVE: &str = r#"(async (q) => {
  const all = (s) => Array.from(document.querySelectorAll(s));
  const matching = (e) => all(e.css).filter((x) => e.text === null || x.textContent === e.text);
  const first = (e) => matching(e)[0] || null;
  switch (q.kind) {
    case "title": return document.title;
    case "location": return location.href;
    case "hash": return location.hash;
    case "count": return matching(q.on).length;
    case "texts": return all(q.css).map((x) => x.textContent);
    case "attribute": { const x = first(q.on); return x === null ? null : x.getAttribute(q.name); }
    case "active": {
      const x = document.activeElement;
      return x === null ? null : { tag: x.tagName.toLowerCase(), id: x.id };
    }
    case "center": {
      for (const x of matching(q.on)) {
        if (x.getBoundingClientRect().width === 0) continue;
        x.scrollIntoView({ block: "center", inline: "center" });
        const r = x.getBoundingClientRect();
        const p = { x: r.left + r.width / 2, y: r.top + r.height / 2 };
        if (r.width > 0 && r.height > 0 && p.x >= 0 && p.y >= 0 && p.x < innerWidth && p.y < innerHeight) return p;
      }
      return null;
    }
    case "focus": { const x = first(q.on); if (x === null) return false; x.focus(); return true; }
    case "clipboard": return await navigator.clipboard.readText();
  }
  return null;
})"#;

/// Whether the page offers a hardware WebGPU adapter: `""`, or why not.
pub(crate) const ADAPTER: &str = r#"(async () => {
  if (!navigator.gpu) return "the page has no WebGPU";
  const a = await navigator.gpu.requestAdapter();
  if (a === null) return "WebGPU offers no adapter";
  if (a.info.isFallbackAdapter !== false) return "WebGPU offers only a software adapter";
  return "";
})()"#;

/// How a text is matched.
#[derive(Clone, Debug, PartialEq)]
pub enum Match {
    Is(String),
    Starts(String),
    Ends(String),
    Contains(String),
}

impl Match {
    pub fn matches(&self, text: &str) -> bool {
        match self {
            Match::Is(t) => text == t,
            Match::Starts(t) => text.starts_with(t.as_str()),
            Match::Ends(t) => text.ends_with(t.as_str()),
            Match::Contains(t) => text.contains(t.as_str()),
        }
    }

    fn describe(&self) -> String {
        match self {
            Match::Is(t) => format!("{t:?}"),
            Match::Starts(t) => format!("starting {t:?}"),
            Match::Ends(t) => format!("ending {t:?}"),
            Match::Contains(t) => format!("containing {t:?}"),
        }
    }
}

/// An element: the first that matches a CSS selector, and has exactly `text` as its text if
/// that's given.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub css: String,
    pub text: Option<String>,
}

impl Element {
    fn json(&self) -> Value {
        json!({"css": self.css, "text": self.text})
    }

    fn describe(&self) -> String {
        match &self.text {
            None => format!("`{}`", self.css),
            Some(t) => format!("`{}` {t:?}", self.css),
        }
    }
}

/// A condition a script waits for.
#[derive(Clone, Debug, PartialEq)]
pub enum Until {
    Title(Match),
    Exists(Element),
    Count(Element, u32),
    Texts(String, Vec<String>),
    Attribute(Element, String, Match),
    Active(String),
    Location(Match),
    Hash(Match),
    Clipboard(Match),
    Printed(String),
    Done,
    All(Vec<Until>),
}

/// A permission a page may be granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    /// Reading and writing the clipboard.
    Clipboard,
}

/// A step of a script.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Wait(Until, Duration),
    Click(Element, Duration),
    ClickAt(f64, f64),
    Press(u32, Mods),
    Insert(String),
    Compose(String, u32, u32),
    Focus(Element, Duration),
    Grant(Vec<Permission>),
    EmulateFocus(bool),
    Size(u32, u32),
}

/// What the page showed for an observation.
#[derive(Clone, Debug, PartialEq)]
pub enum Seen {
    Text(String),
    Texts(Vec<String>),
    Count(u32),
    Attribute(Option<String>),
    Active(Option<(String, String)>),
}

/// An observation of a page.
#[derive(Clone, Debug, PartialEq)]
pub enum Query {
    Title,
    Location,
    Hash,
    Count(Element),
    Texts(String),
    Attribute(Element, String),
    Active,
    Clipboard,
}

impl Query {
    fn json(&self) -> Value {
        match self {
            Query::Title => json!({"kind": "title"}),
            Query::Location => json!({"kind": "location"}),
            Query::Hash => json!({"kind": "hash"}),
            Query::Count(e) => json!({"kind": "count", "on": e.json()}),
            Query::Texts(css) => json!({"kind": "texts", "css": css}),
            Query::Attribute(e, name) => json!({"kind": "attribute", "on": e.json(), "name": name}),
            Query::Active => json!({"kind": "active"}),
            Query::Clipboard => json!({"kind": "clipboard"}),
        }
    }
}

/// What `query` reads of the page, by `deadline`.
pub fn observe(tab: &Tab, query: &Query, deadline: Instant) -> Result<Seen, String> {
    let v = tab.evaluate(&format!("{OBSERVE}({})", query.json()), deadline)?;
    let text = |v: &Value| v.as_str().map(str::to_string).ok_or("the page answered something else");
    Ok(match query {
        Query::Title | Query::Location | Query::Hash | Query::Clipboard => Seen::Text(text(&v)?),
        Query::Count(_) => {
            Seen::Count(v.as_u64().ok_or("the page answered something else")? as u32)
        }
        Query::Texts(_) => Seen::Texts(
            v.as_array()
                .ok_or("the page answered something else")?
                .iter()
                .map(text)
                .collect::<Result<_, _>>()?,
        ),
        Query::Attribute(..) => Seen::Attribute(v.as_str().map(str::to_string)),
        Query::Active => Seen::Active(match &v {
            Value::Null => None,
            o => Some((text(&o["tag"])?, text(&o["id"])?)),
        }),
    })
}

/// Reads a script in std's format.
pub fn parse(text: &str) -> Result<Vec<Step>, String> {
    let v: Value =
        serde_json::from_str(text).map_err(|e| format!("a browser script isn't JSON: {e}"))?;
    let steps = v.as_array().ok_or("a browser script is a JSON array of steps")?;
    steps
        .iter()
        .enumerate()
        .map(|(i, s)| step(s).map_err(|why| format!("the script's step {}: {why}", i + 1)))
        .collect()
}

fn string(v: &Value, key: &str) -> Result<String, String> {
    v[key].as_str().map(str::to_string).ok_or_else(|| format!("`{key}` is text"))
}

fn whole(v: &Value, key: &str) -> Result<u32, String> {
    v[key]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("`{key}` is a whole number"))
}

fn number(v: &Value, key: &str) -> Result<f64, String> {
    v[key].as_f64().filter(|x| x.is_finite()).ok_or_else(|| format!("`{key}` is a finite number"))
}

fn seconds(v: &Value) -> Result<Duration, String> {
    let s = number(v, "timeout")?;
    if !(s > 0.0 && s <= 600.0) {
        return Err(format!("a timeout is more than 0 and at most 600 s, not {s}"));
    }
    Ok(Duration::from_secs_f64(s))
}

fn element(v: &Value) -> Result<Element, String> {
    let css = string(v, "css")?;
    if css.is_empty() {
        return Err("an element's selector is empty".into());
    }
    let text = match &v["text"] {
        Value::Null => None,
        t => Some(t.as_str().ok_or("an element's `text` is text")?.to_string()),
    };
    Ok(Element { css, text })
}

fn matcher(v: &Value) -> Result<Match, String> {
    let o = v
        .as_object()
        .filter(|o| o.len() == 1)
        .ok_or("a match is one of is, starts, ends, contains")?;
    let (k, t) = o.iter().next().expect("one");
    let t = t.as_str().ok_or("a match's text is text")?.to_string();
    Ok(match k.as_str() {
        "is" => Match::Is(t),
        "starts" => Match::Starts(t),
        "ends" => Match::Ends(t),
        "contains" => Match::Contains(t),
        k => return Err(format!("`{k}` isn't a match")),
    })
}

fn until(v: &Value) -> Result<Until, String> {
    let o = v.as_object().ok_or("a condition is an object")?;
    let on = |k: &str| element(&v[k]);
    Ok(if o.contains_key("title") {
        Until::Title(matcher(&v["title"])?)
    } else if o.contains_key("exists") {
        Until::Exists(on("exists")?)
    } else if o.contains_key("count") {
        Until::Count(on("count")?, whole(v, "n")?)
    } else if o.contains_key("texts") {
        let are = v["are"].as_array().ok_or("`are` is an array of texts")?;
        let are = are
            .iter()
            .map(|t| t.as_str().map(str::to_string).ok_or("`are` holds texts"))
            .collect::<Result<_, _>>()?;
        Until::Texts(string(v, "texts")?, are)
    } else if o.contains_key("attribute") {
        Until::Attribute(on("attribute")?, string(v, "name")?, matcher(&v["is"])?)
    } else if o.contains_key("active") {
        Until::Active(string(v, "active")?.to_ascii_lowercase())
    } else if o.contains_key("location") {
        Until::Location(matcher(&v["location"])?)
    } else if o.contains_key("hash") {
        Until::Hash(matcher(&v["hash"])?)
    } else if o.contains_key("clipboard") {
        Until::Clipboard(matcher(&v["clipboard"])?)
    } else if o.contains_key("printed") {
        Until::Printed(string(v, "printed")?)
    } else if o.contains_key("done") {
        Until::Done
    } else if o.contains_key("all") {
        let all = v["all"].as_array().ok_or("`all` is an array of conditions")?;
        Until::All(all.iter().map(until).collect::<Result<_, _>>()?)
    } else {
        return Err(format!("{v} isn't a condition"));
    })
}

fn step(v: &Value) -> Result<Step, String> {
    let kind = string(v, "step")?;
    Ok(match kind.as_str() {
        "wait" => Step::Wait(until(&v["until"])?, seconds(v)?),
        "click" => Step::Click(element(&v["on"])?, seconds(v)?),
        "click_at" => Step::ClickAt(number(v, "x")?, number(v, "y")?),
        "press" => {
            let flag = |k: &str| v[k].as_bool().unwrap_or(false);
            let mods = Mods {
                shift: flag("shift"),
                ctrl: flag("ctrl"),
                alt: flag("alt"),
                meta: flag("meta"),
            };
            let key = whole(v, "key")?;
            // Refused now, not when the step runs.
            crate::keys::press(key, mods)?;
            Step::Press(key, mods)
        }
        "insert" => Step::Insert(string(v, "text")?),
        "compose" => Step::Compose(string(v, "text")?, whole(v, "start")?, whole(v, "end")?),
        "focus" => Step::Focus(element(&v["on"])?, seconds(v)?),
        "grant" => {
            let p = v["permissions"].as_array().ok_or("`permissions` is an array")?;
            let p = p
                .iter()
                .map(|p| match p.as_str() {
                    Some("clipboard") => Ok(Permission::Clipboard),
                    _ => Err(format!("{p} isn't a permission")),
                })
                .collect::<Result<_, _>>()?;
            Step::Grant(p)
        }
        "emulate_focus" => Step::EmulateFocus(v["on"].as_bool().ok_or("`on` is true or false")?),
        "size" => {
            let (w, h) = (whole(v, "width")?, whole(v, "height")?);
            if !(1..=16384).contains(&w) || !(1..=16384).contains(&h) {
                return Err(format!("a size of {w}×{h} isn't 1 to 16384 each way"));
            }
            Step::Size(w, h)
        }
        k => return Err(format!("`{k}` isn't a step")),
    })
}

/// Whether `why`, an evaluation's failure, is a page between documents: no context to evaluate
/// in yet, or the one it had went.
fn between_documents(why: &str) -> bool {
    why.contains("execution context") || why.contains("Execution context")
}

impl Until {
    /// Whether the condition holds now, and what was seen of each part that doesn't.
    fn check(&self, tab: &Tab, deadline: Instant) -> Result<(bool, String), String> {
        let read = |q: Query| -> Result<Option<Seen>, String> {
            match observe(tab, &q, deadline) {
                Ok(s) => Ok(Some(s)),
                Err(why) if between_documents(&why) => Ok(None),
                Err(why) => Err(why),
            }
        };
        let text = |s: Option<Seen>| match s {
            Some(Seen::Text(t)) => Some(t),
            _ => None,
        };
        // Whether text `t` matches `m`, and `what` it was.
        let matched = |m: &Match, t: Option<String>, what: &str| {
            let shown = t.as_ref().map_or("nothing yet".into(), |t| format!("{t:?}"));
            (t.is_some_and(|t| m.matches(&t)), format!("{what} {shown}"))
        };
        Ok(match self {
            Until::Title(m) => matched(m, text(read(Query::Title)?), "the title was"),
            Until::Location(m) => matched(m, text(read(Query::Location)?), "the location was"),
            Until::Hash(m) => matched(m, text(read(Query::Hash)?), "the location's hash was"),
            // A page that may not read the clipboard yet (it hasn't the focus) holds nothing.
            Until::Clipboard(m) => {
                matched(m, read(Query::Clipboard).ok().and_then(text), "the clipboard held")
            }
            Until::Exists(e) | Until::Count(e, _) => {
                let n = match read(Query::Count(e.clone()))? {
                    Some(Seen::Count(n)) => n,
                    _ => 0,
                };
                let held = match self {
                    Until::Count(_, want) => n == *want,
                    _ => n > 0,
                };
                (held, format!("{n} matched {}", e.describe()))
            }
            Until::Texts(css, are) => {
                let t = match read(Query::Texts(css.clone()))? {
                    Some(Seen::Texts(t)) => t,
                    _ => Vec::new(),
                };
                (&t == are, format!("`{css}` had {t:?}"))
            }
            Until::Attribute(e, name, m) => {
                let a = match read(Query::Attribute(e.clone(), name.clone()))? {
                    Some(Seen::Attribute(a)) => a,
                    _ => None,
                };
                matched(m, a, &format!("{}'s `{name}` was", e.describe()))
            }
            Until::Active(tag) => {
                let a = match read(Query::Active)? {
                    Some(Seen::Active(a)) => a,
                    _ => None,
                };
                let what = a.as_ref().map_or("nothing".into(), |(t, _)| format!("a `{t}`"));
                (a.is_some_and(|(t, _)| &t == tag), format!("{what} had the focus"))
            }
            Until::Printed(line) => {
                let printed = tab.printed()?;
                let last = printed.last().map_or("nothing".into(), |l| format!("{l:?}"));
                (printed.iter().any(|l| l == line), format!("the program last printed {last}"))
            }
            Until::Done => {
                if !tab.knows_done() {
                    return Err(
                        "this page's results reach no server that tells the driver of them".into(),
                    );
                }
                match tab.done() {
                    // A result that can't be read whole is no result to wait for.
                    Some(Err(why)) => return Err(why),
                    d => (d.is_some(), "no server has kept a results/DONE of the page's".into()),
                }
            }
            Until::All(parts) => {
                let mut seen = Vec::new();
                for p in parts {
                    let (held, what) = p.check(tab, deadline)?;
                    if !held {
                        seen.push(what);
                    }
                }
                (seen.is_empty(), seen.join("; "))
            }
        })
    }

    /// What the condition asks for, in words.
    pub fn describe(&self) -> String {
        match self {
            Until::Title(m) => format!("the title {}", m.describe()),
            Until::Exists(e) => format!("an element {}", e.describe()),
            Until::Count(e, n) => format!("{n} elements {}", e.describe()),
            Until::Texts(css, are) => format!("`{css}`'s texts {are:?}"),
            Until::Attribute(e, name, m) => format!("{}'s `{name}` {}", e.describe(), m.describe()),
            Until::Active(tag) => format!("a `{tag}` with the focus"),
            Until::Location(m) => format!("the location {}", m.describe()),
            Until::Hash(m) => format!("the location's hash {}", m.describe()),
            Until::Clipboard(m) => format!("the clipboard holding {}", m.describe()),
            Until::Printed(l) => format!("the program printing {l:?}"),
            Until::Done => "the page's results/DONE".into(),
            Until::All(parts) => {
                parts.iter().map(Until::describe).collect::<Vec<_>>().join(" and ")
            }
        }
    }
}

/// How often a wait looks again when nothing new has been seen.
const LOOK: Duration = Duration::from_millis(50);

/// Waits until `until` holds, for at most `limit` and not past `deadline` (the script's),
/// stopping early when `stop` says why.
fn wait(
    tab: &Tab,
    until: &Until,
    deadline: Instant,
    limit: Duration,
    stop: &dyn Fn() -> Option<String>,
) -> Result<(), String> {
    let own = Instant::now() + limit;
    let due = own.min(deadline);
    loop {
        if let Some(why) = stop() {
            return Err(why);
        }
        // What a wait reads may be kept on this side (what the program printed, the page's
        // result): a browser that has closed has nothing more coming.
        if let Some(why) = tab.ended() {
            return Err(format!("the browser closed: {why}"));
        }
        if tab.crashed() {
            return Err("the page crashed".into());
        }
        let call = (Instant::now() + Duration::from_secs(10)).min(due.max(Instant::now() + LOOK));
        let (held, seen) = until.check(tab, call)?;
        if held {
            return Ok(());
        }
        if Instant::now() >= due {
            let by = if own <= deadline {
                format!("within {} s", limit.as_secs_f64())
            } else {
                "by the script's deadline".into()
            };
            return Err(format!("the page never had {} {by}: {seen}", until.describe()));
        }
        tab.wait_for_change(LOOK.min(due.saturating_duration_since(Instant::now())));
    }
}

/// Runs `steps` on `tab`, in order, by `deadline`, looking at `stop` as it waits: why it
/// stopped, if it didn't finish.
pub fn run(
    tab: &Tab,
    steps: &[Step],
    deadline: Instant,
    stop: &dyn Fn() -> Option<String>,
) -> Result<(), String> {
    for (i, s) in steps.iter().enumerate() {
        if let Some(why) = stop() {
            return Err(why);
        }
        if Instant::now() >= deadline {
            return Err(format!("the script ran past its deadline before step {}", i + 1));
        }
        one(tab, s, deadline, stop).map_err(|why| format!("step {}: {why}", i + 1))?;
    }
    Ok(())
}

fn click(tab: &Tab, x: f64, y: f64, deadline: Instant) -> Result<(), String> {
    for kind in ["mousePressed", "mouseReleased"] {
        let buttons = if kind == "mousePressed" { 1 } else { 0 };
        tab.call(
            "Input.dispatchMouseEvent",
            json!({"type": kind, "x": x, "y": y, "button": "left", "buttons": buttons, "clickCount": 1}),
            deadline,
        )?;
    }
    Ok(())
}

fn one(
    tab: &Tab,
    s: &Step,
    deadline: Instant,
    stop: &dyn Fn() -> Option<String>,
) -> Result<(), String> {
    let within = |d: Duration| (Instant::now() + d).min(deadline);
    match s {
        Step::Wait(until, timeout) => wait(tab, until, deadline, *timeout, stop),
        Step::Focus(e, timeout) => {
            wait(tab, &Until::Exists(e.clone()), deadline, *timeout, stop)?;
            let q = json!({"kind": "focus", "on": e.json()});
            match tab.evaluate(&format!("{OBSERVE}({q})"), deadline)? {
                Value::Bool(true) => Ok(()),
                _ => Err(format!("no element {} to focus", e.describe())),
            }
        }
        Step::Click(e, timeout) => {
            // A person clicks what they can see: the first match of some size, in the viewport
            // once scrolled into view. The click lands where it is, on whatever is there: an
            // invisible node of a program's semantic tree lies where the program draws it, and
            // the program's canvas, under it, takes the click if the node doesn't.
            let due = within(*timeout);
            let q = json!({"kind": "center", "on": e.json()});
            loop {
                if let Some(why) = stop() {
                    return Err(why);
                }
                if let Some(why) = tab.ended() {
                    return Err(format!("the browser closed: {why}"));
                }
                let at = match tab
                    .evaluate(&format!("{OBSERVE}({q})"), due.max(Instant::now() + LOOK))
                {
                    Ok(at) => at,
                    Err(why) if between_documents(&why) => Value::Null,
                    Err(why) => return Err(why),
                };
                if let (Some(x), Some(y)) = (at["x"].as_f64(), at["y"].as_f64()) {
                    return click(tab, x, y, deadline);
                }

                if Instant::now() >= due {
                    return Err(format!(
                        "no element {} of some size in the viewport within {} s",
                        e.describe(),
                        timeout.as_secs_f64()
                    ));
                }
                tab.wait_for_change(LOOK);
            }
        }
        Step::ClickAt(x, y) => click(tab, *x, *y, deadline),
        Step::Press(key, mods) => {
            let (down, up) = crate::keys::press(*key, *mods)?;
            tab.call("Input.dispatchKeyEvent", down, deadline)?;
            tab.call("Input.dispatchKeyEvent", up, deadline).map(|_| ())
        }
        Step::Insert(text) => {
            tab.call("Input.insertText", json!({"text": text}), deadline).map(|_| ())
        }
        Step::Compose(text, start, end) => tab
            .call(
                "Input.imeSetComposition",
                json!({"text": text, "selectionStart": start, "selectionEnd": end}),
                deadline,
            )
            .map(|_| ()),
        Step::Grant(permissions) => {
            let mut names = Vec::new();
            for p in permissions {
                match p {
                    Permission::Clipboard => {
                        names.extend(["clipboardReadWrite", "clipboardSanitizedWrite"])
                    }
                }
            }
            tab.call_browser("Browser.grantPermissions", json!({"permissions": names}), deadline)
                .map(|_| ())
        }
        Step::EmulateFocus(on) => tab
            .call("Emulation.setFocusEmulationEnabled", json!({"enabled": on}), deadline)
            .map(|_| ()),
        Step::Size(w, h) => tab.resize(*w, *h, deadline),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_reads_as_its_typed_steps() {
        let text = r##"[
          {"step": "wait", "until": {"all": [{"title": {"is": "Wrela docs"}},
                                             {"count": {"css": "a", "text": "6. Memory"}, "n": 2}]}, "timeout": 30},
          {"step": "click", "on": {"css": "a[href$=\"#io\"]"}, "timeout": 20},
          {"step": "click_at", "x": 20, "y": 30.5},
          {"step": "press", "key": 1, "shift": true},
          {"step": "insert", "text": "é"},
          {"step": "compose", "text": "かな", "start": 2, "end": 2},
          {"step": "grant", "permissions": ["clipboard"]},
          {"step": "emulate_focus", "on": true},
          {"step": "size", "width": 64, "height": 48},
          {"step": "wait", "until": {"texts": "nav a", "are": ["6.1", "6.2"]}, "timeout": 1},
          {"step": "wait", "until": {"attribute": {"css": "a"}, "name": "href", "is": {"ends": "#next"}}, "timeout": 1},
          {"step": "wait", "until": {"printed": "text a 1"}, "timeout": 1},
          {"step": "wait", "until": {"done": true}, "timeout": 1}
        ]"##;
        let steps = parse(text).expect("a script");
        assert_eq!(steps.len(), 13);
        let memory = Element { css: "a".into(), text: Some("6. Memory".into()) };
        assert_eq!(
            steps[0],
            Step::Wait(
                Until::All(vec![
                    Until::Title(Match::Is("Wrela docs".into())),
                    Until::Count(memory, 2)
                ]),
                Duration::from_secs(30)
            )
        );
        assert_eq!(steps[2], Step::ClickAt(20.0, 30.5));
        assert_eq!(steps[3], Step::Press(1, Mods { shift: true, ..Mods::default() }));
        assert_eq!(steps[6], Step::Grant(vec![Permission::Clipboard]));
        assert_eq!(
            steps[10],
            Step::Wait(
                Until::Attribute(
                    Element { css: "a".into(), text: None },
                    "href".into(),
                    Match::Ends("#next".into())
                ),
                Duration::from_secs(1)
            )
        );
    }

    #[test]
    fn a_bad_step_is_refused_with_where_it_is() {
        for (bad, why) in [
            (r#"[{"step": "jump"}]"#, "`jump` isn't a step"),
            (r#"[{"step": "press", "key": 0}]"#, "isn't a key's number"),
            (r#"[{"step": "wait", "until": {"title": {"is": "x"}}, "timeout": 0}]"#, "more than 0"),
            (r#"[{"step": "wait", "until": {"smell": 1}, "timeout": 1}]"#, "isn't a condition"),
            (r#"[{"step": "click", "on": {"css": ""}, "timeout": 1}]"#, "selector is empty"),
            (r#"[{"step": "grant", "permissions": ["camera"]}]"#, "isn't a permission"),
            (r#"{"step": "wait"}"#, "a JSON array"),
        ] {
            let e = parse(bad).expect_err(bad);
            assert!(e.contains(why), "{bad}: {e}");
        }
        let e = parse(
            r#"[{"step": "insert", "text": "a"}, {"step": "size", "width": 0, "height": 1}]"#,
        );
        assert!(e.expect_err("a zero size").starts_with("the script's step 2:"));
    }

    #[test]
    fn matches_compare_exactly_or_by_their_ends() {
        assert!(Match::Is("ab".into()).matches("ab") && !Match::Is("ab".into()).matches("abc"));
        assert!(Match::Starts("# std::io\n\n".into()).matches("# std::io\n\nRequests"));
        assert!(Match::Ends("#next".into()).matches("http://x/#next"));
        assert!(
            Match::Contains("b".into()).matches("abc")
                && !Match::Contains("d".into()).matches("abc")
        );
    }
}
