//! # Input
//!
//! A program reads pointer and keyboard events through the import
//! `wrela.input(ptr: i32, cap: i32) -> i32` (language.md's `std::input`): the host copies up to
//! `cap` events, oldest first, into the program's memory at `ptr`, [`EVENT_SIZE`] bytes each,
//! and returns how many it copied. Events it didn't copy stay queued for the next call. A host
//! gives a call only the events that arrived before the call started, so what a call reads
//! doesn't depend on how long it runs.
//!
//! ## An event
//!
//! Six little-endian words:
//!
//! | Word | Field |
//! |---|---|
//! | 0 | the kind ([`EventKind`]) |
//! | 1 | the modifiers held: bit 0 Shift, 1 Control, 2 Alt, 3 Meta |
//! | 2–5 | `a`, `b`, `c`, `d`, by kind |
//!
//! | Kind | `a`, `b` | `c`, `d` |
//! |---|---|---|
//! | `PointerMove` | x, y: f32, pixels from the top left, in the canvas's pixels | 0, 0 |
//! | `PointerDown`, `PointerUp` | x, y | the button (`c`: 0 primary, 1 middle, 2 secondary), 0 |
//! | `Wheel` | x, y | the scroll, f32 pixels: right and down are positive |
//! | `KeyDown` | the key ([`KEYS`]), 1 if it's the key repeating | 0, 0 |
//! | `KeyUp` | the key, 0 | 0, 0 |
//! | `Text` | a Unicode scalar value typed, 0 | 0, 0 |
//!
//! Positions are in the same pixels as `frame`'s width and height. A key is a physical key
//! (DOM's `KeyboardEvent.code`), named as [`KEYS`] lists it; one the list doesn't name is 0.
//! What a key types arrives separately, as `Text`.
//!
//! ## Scripted input
//!
//! A script drives a program's input for tests and headless runs, the same way in both hosts:
//! a JSON array of events, each with the frame it arrives before.
//!
//! ```json
//! [
//!   { "frame": 0, "type": "move", "x": 100, "y": 200 },
//!   { "frame": 1, "type": "down", "x": 100, "y": 200, "button": "primary" },
//!   { "frame": 3, "type": "up", "x": 140, "y": 200 },
//!   { "frame": 4, "type": "wheel", "x": 140, "y": 200, "dx": 0, "dy": 120 },
//!   { "frame": 5, "type": "key", "key": "Tab", "shift": true },
//!   { "frame": 6, "type": "text", "text": "Grüße" }
//! ]
//! ```
//!
//! `type` is `move`, `down`, `up`, `wheel`, `keydown`, `keyup`, `key` (a key down then up),
//! or `text` (a `Text` event for each character); `button` defaults to `primary`; `shift`,
//! `ctrl`, `alt` and `meta` default to false. [`parse_script`] reads one.
//!
//! An event has the frame it arrives before (`"frame": n`), or instead the tick it's a record
//! of (`"tick": n`, language.md's `std::tick`); each kind's numbers don't decrease.
//! - Before calling `frame` for frame `i`, a host queues every event of frame `i`, in order, for
//!   the program's `std::input::events()`; a program with a ticker gets them as records too,
//!   which its next tick takes (in lockstep, the first tick that runs for frame `i`).
//! - A tick's events are tick `n`'s records, and nothing else: the frames never see them.

use serde_json::Value;

/// `wrela.input(ptr, cap) -> count`.
pub const IMPORT_INPUT: &str = "input";
/// Bytes per event: six words.
pub const EVENT_SIZE: u32 = 24;

/// What an event is (word 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum EventKind {
    PointerMove = 1,
    PointerDown = 2,
    PointerUp = 3,
    Wheel = 4,
    KeyDown = 5,
    KeyUp = 6,
    Text = 7,
}

impl EventKind {
    pub const ALL: [EventKind; 7] = [
        EventKind::PointerMove,
        EventKind::PointerDown,
        EventKind::PointerUp,
        EventKind::Wheel,
        EventKind::KeyDown,
        EventKind::KeyUp,
        EventKind::Text,
    ];

    pub fn name(self) -> &'static str {
        match self {
            EventKind::PointerMove => "PointerMove",
            EventKind::PointerDown => "PointerDown",
            EventKind::PointerUp => "PointerUp",
            EventKind::Wheel => "Wheel",
            EventKind::KeyDown => "KeyDown",
            EventKind::KeyUp => "KeyUp",
            EventKind::Text => "Text",
        }
    }
}

/// The modifier bits (word 1).
pub const SHIFT: u32 = 1;
pub const CONTROL: u32 = 2;
pub const ALT: u32 = 4;
pub const META: u32 = 8;

/// The buttons (`c` of a pointer down or up).
pub const BUTTONS: [&str; 3] = ["primary", "middle", "secondary"];

/// The physical keys, by number: index `i` is key `i` (DOM's `KeyboardEvent.code`). 0 is a key
/// this list doesn't name. Numbers never change meaning; new keys go at the end.
pub const KEYS: [&str; 90] = [
    "Unknown",
    "KeyA",
    "KeyB",
    "KeyC",
    "KeyD",
    "KeyE",
    "KeyF",
    "KeyG",
    "KeyH",
    "KeyI",
    "KeyJ",
    "KeyK",
    "KeyL",
    "KeyM",
    "KeyN",
    "KeyO",
    "KeyP",
    "KeyQ",
    "KeyR",
    "KeyS",
    "KeyT",
    "KeyU",
    "KeyV",
    "KeyW",
    "KeyX",
    "KeyY",
    "KeyZ",
    "Digit0",
    "Digit1",
    "Digit2",
    "Digit3",
    "Digit4",
    "Digit5",
    "Digit6",
    "Digit7",
    "Digit8",
    "Digit9",
    "Enter",
    "Escape",
    "Backspace",
    "Tab",
    "Space",
    "ArrowLeft",
    "ArrowRight",
    "ArrowUp",
    "ArrowDown",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Insert",
    "Delete",
    "ShiftLeft",
    "ShiftRight",
    "ControlLeft",
    "ControlRight",
    "AltLeft",
    "AltRight",
    "MetaLeft",
    "MetaRight",
    "CapsLock",
    "Minus",
    "Equal",
    "BracketLeft",
    "BracketRight",
    "Backslash",
    "Semicolon",
    "Quote",
    "Backquote",
    "Comma",
    "Period",
    "Slash",
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12",
    "NumpadEnter",
    "NumpadAdd",
    "NumpadSubtract",
    "NumpadMultiply",
    "NumpadDivide",
    "NumpadDecimal",
];

/// A key's number, by its DOM code; 0 for one [`KEYS`] doesn't name.
pub fn key_code(name: &str) -> u32 {
    KEYS.iter().position(|k| *k == name).unwrap_or(0) as u32
}

/// One event, as the program reads it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Event {
    pub kind: EventKind,
    pub modifiers: u32,
    /// `a`, `b`, `c`, `d`: each word's bits.
    pub words: [u32; 4],
}

impl Event {
    pub fn pointer(kind: EventKind, x: f32, y: f32, button: u32, modifiers: u32) -> Event {
        Event { kind, modifiers, words: [x.to_bits(), y.to_bits(), button, 0] }
    }

    pub fn wheel(x: f32, y: f32, dx: f32, dy: f32, modifiers: u32) -> Event {
        let words = [x.to_bits(), y.to_bits(), dx.to_bits(), dy.to_bits()];
        Event { kind: EventKind::Wheel, modifiers, words }
    }

    pub fn key(down: bool, key: u32, repeat: bool, modifiers: u32) -> Event {
        let kind = if down { EventKind::KeyDown } else { EventKind::KeyUp };
        Event { kind, modifiers, words: [key, u32::from(repeat && down), 0, 0] }
    }

    pub fn text(c: char) -> Event {
        Event { kind: EventKind::Text, modifiers: 0, words: [c as u32, 0, 0, 0] }
    }

    /// Its [`EVENT_SIZE`] bytes, as the program reads them.
    pub fn bytes(&self) -> [u8; EVENT_SIZE as usize] {
        let mut out = [0u8; EVENT_SIZE as usize];
        let words = [self.kind as u32, self.modifiers].into_iter().chain(self.words);
        for (i, w) in words.enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
        }
        out
    }
}

/// When a script's event arrives: before a frame, or as a tick's record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    Frame(u32),
    Tick(u32),
}

/// A script's event, and when it arrives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scripted {
    pub at: At,
    pub event: Event,
}

/// The events of `script` that arrive before frame `frame`.
pub fn events_at(script: &[Scripted], frame: u32) -> impl Iterator<Item = Event> + '_ {
    script.iter().filter(move |s| s.at == At::Frame(frame)).map(|s| s.event)
}

/// The events of `script` that are tick `tick`'s records.
pub fn records_at(script: &[Scripted], tick: u32) -> impl Iterator<Item = Event> + '_ {
    script.iter().filter(move |s| s.at == At::Tick(tick)).map(|s| s.event)
}

/// Reads a script ([the format](self#scripted-input)): its events, in order, or why it isn't
/// one.
pub fn parse_script(text: &str) -> Result<Vec<Scripted>, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("the script isn't JSON: {e}"))?;
    let items = value.as_array().ok_or("a script is a JSON array of events")?;
    let mut out = Vec::new();
    let (mut last_frame, mut last_tick) = (0, 0);
    for (i, item) in items.iter().enumerate() {
        let at = |why: &str| format!("event {i}: {why}");
        let o = item.as_object().ok_or_else(|| at("an event is a JSON object"))?;
        let number = |key: &str| {
            o.get(key).map(|v| v.as_u64().filter(|&f| f <= u64::from(u32::MAX)).map(|f| f as u32))
        };
        let when = match (number("frame"), number("tick")) {
            (Some(Some(f)), None) => {
                if f < last_frame {
                    return Err(at("frames don't decrease"));
                }
                last_frame = f;
                At::Frame(f)
            }
            (None, Some(Some(t))) => {
                if t < last_tick {
                    return Err(at("ticks don't decrease"));
                }
                last_tick = t;
                At::Tick(t)
            }
            (Some(_), Some(_)) => return Err(at("an event has a `frame` or a `tick`, not both")),
            _ => return Err(at("`frame` (or `tick`) must be a whole number")),
        };
        let num = |key: &str| -> Result<f32, String> {
            match o.get(key) {
                None => Ok(0.0),
                Some(v) => v
                    .as_f64()
                    .map(|x| x as f32)
                    .filter(|x| x.is_finite())
                    .ok_or_else(|| at(&format!("`{key}` must be a number"))),
            }
        };
        let flag = |key: &str| -> Result<bool, String> {
            match o.get(key) {
                None => Ok(false),
                Some(v) => v.as_bool().ok_or_else(|| at(&format!("`{key}` must be true or false"))),
            }
        };
        let modifiers = [("shift", SHIFT), ("ctrl", CONTROL), ("alt", ALT), ("meta", META)]
            .iter()
            .try_fold(0, |m, (k, bit)| flag(k).map(|on| if on { m | bit } else { m }))?;
        let ty = o.get("type").and_then(Value::as_str).ok_or_else(|| at("`type` is missing"))?;
        let button = || -> Result<u32, String> {
            match o.get("button") {
                None => Ok(0),
                Some(v) => v
                    .as_str()
                    .and_then(|b| BUTTONS.iter().position(|x| *x == b))
                    .map(|b| b as u32)
                    .ok_or_else(|| at("`button` is primary, middle or secondary")),
            }
        };
        let key = || -> Result<u32, String> {
            let name =
                o.get("key").and_then(Value::as_str).ok_or_else(|| at("`key` is missing"))?;
            match key_code(name) {
                0 => Err(at(&format!("`{name}` isn't a key (they're DOM codes, such as KeyA)"))),
                k => Ok(k),
            }
        };
        let mut push = |event| out.push(Scripted { at: when, event });
        match ty {
            "move" => {
                push(Event::pointer(EventKind::PointerMove, num("x")?, num("y")?, 0, modifiers))
            }
            "down" | "up" => {
                let kind = if ty == "down" { EventKind::PointerDown } else { EventKind::PointerUp };
                push(Event::pointer(kind, num("x")?, num("y")?, button()?, modifiers));
            }
            "wheel" => push(Event::wheel(num("x")?, num("y")?, num("dx")?, num("dy")?, modifiers)),
            "keydown" => push(Event::key(true, key()?, flag("repeat")?, modifiers)),
            "keyup" => push(Event::key(false, key()?, false, modifiers)),
            "key" => {
                let k = key()?;
                push(Event::key(true, k, false, modifiers));
                push(Event::key(false, k, false, modifiers));
            }
            "text" => {
                let t =
                    o.get("text").and_then(Value::as_str).ok_or_else(|| at("`text` is missing"))?;
                for c in t.chars() {
                    push(Event::text(c));
                }
            }
            other => {
                return Err(at(&format!(
                    "`{other}` isn't a type (move, down, up, wheel, keydown, keyup, key or text)"
                )));
            }
        }
    }
    Ok(out)
}

/// A host's queue of events not yet read: [`Queue::take`] gives `wrela.input` its events.
#[derive(Clone, Debug, Default)]
pub struct Queue {
    events: std::collections::VecDeque<Event>,
}

impl Queue {
    pub fn push(&mut self, e: Event) {
        self.events.push_back(e);
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Up to `cap` events, oldest first, as bytes; the rest stay queued.
    pub fn take(&mut self, cap: u32) -> Vec<u8> {
        let n = (cap as usize).min(self.events.len());
        self.events.drain(..n).flat_map(|e| e.bytes()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind of event a script can hold, read back as the program would read it.
    #[test]
    fn scripts_read_back() {
        let script = r#"[
            {"frame": 0, "type": "move", "x": 10.5, "y": 20},
            {"frame": 1, "type": "down", "x": 10.5, "y": 20, "button": "secondary", "shift": true},
            {"frame": 1, "type": "up", "x": 11, "y": 21},
            {"frame": 2, "type": "wheel", "x": 1, "y": 2, "dx": -3, "dy": 120},
            {"frame": 3, "type": "key", "key": "Tab", "ctrl": true, "meta": true},
            {"frame": 3, "type": "keydown", "key": "KeyA", "repeat": true},
            {"frame": 4, "type": "text", "text": "aé"}
        ]"#;
        let s = parse_script(script).expect("a valid script");
        assert_eq!(s.len(), 9);
        assert_eq!(s[0].event, Event::pointer(EventKind::PointerMove, 10.5, 20.0, 0, 0));
        assert_eq!(s[1].event.words[2], 2);
        assert_eq!(s[1].event.modifiers, SHIFT);
        assert_eq!(s[3].event, Event::wheel(1.0, 2.0, -3.0, 120.0, 0));
        assert_eq!(s[4].event.kind, EventKind::KeyDown);
        assert_eq!(s[4].event.words[0], key_code("Tab"));
        assert_eq!(s[4].event.modifiers, CONTROL | META);
        assert_eq!(s[5].event.kind, EventKind::KeyUp);
        assert_eq!(s[6].event.words, [key_code("KeyA"), 1, 0, 0]);
        assert_eq!(s[8].event, Event::text('é'));
        assert_eq!(s[8].at, At::Frame(4));
        let bytes = s[3].event.bytes();
        assert_eq!(&bytes[..4], &4u32.to_le_bytes());
        assert_eq!(&bytes[20..], &120.0f32.to_bits().to_le_bytes());
    }

    #[test]
    fn bad_scripts_say_why() {
        let bad = [
            ("{}", "a script is a JSON array"),
            (r#"[{"type": "move"}]"#, "`frame` (or `tick`) must be a whole number"),
            (r#"[{"frame": 2, "type": "move"}, {"frame": 1, "type": "move"}]"#, "don't decrease"),
            (r#"[{"tick": 2, "type": "move"}, {"tick": 1, "type": "move"}]"#, "don't decrease"),
            (r#"[{"tick": 2, "frame": 1, "type": "move"}]"#, "not both"),
            (r#"[{"frame": 0, "type": "jump"}]"#, "isn't a type"),
            (r#"[{"frame": 0, "type": "key", "key": "A"}]"#, "isn't a key"),
            (r#"[{"frame": 0, "type": "down", "button": "left"}]"#, "`button` is primary"),
        ];
        for (text, why) in bad {
            let e = parse_script(text).expect_err(text);
            assert!(e.contains(why), "{text}: {e}");
        }
    }

    /// Tick-keyed events are a tick's records, and frames never see them.
    #[test]
    fn ticks_events_are_records() {
        let script = r#"[
            {"tick": 3, "type": "key", "key": "Space"},
            {"frame": 0, "type": "move", "x": 1, "y": 2},
            {"tick": 5, "type": "text", "text": "z"}
        ]"#;
        let s = parse_script(script).expect("a valid script");
        assert_eq!(s.len(), 4);
        assert_eq!(records_at(&s, 3).count(), 2);
        assert_eq!(records_at(&s, 5).collect::<Vec<_>>(), [Event::text('z')]);
        assert_eq!(events_at(&s, 0).count(), 1);
        assert_eq!(events_at(&s, 3).count(), 0);
    }

    #[test]
    fn the_queue_keeps_what_it_didnt_give() {
        let mut q = Queue::default();
        for c in "abc".chars() {
            q.push(Event::text(c));
        }
        assert_eq!(q.take(2).len(), 2 * EVENT_SIZE as usize);
        assert_eq!(q.len(), 1);
        assert_eq!(q.take(8), Event::text('c').bytes());
        assert!(q.is_empty());
    }

    /// Key numbers are a stable table: no name twice, and the first is "no key".
    #[test]
    fn keys_are_unique() {
        assert_eq!(KEYS[0], "Unknown");
        for (i, k) in KEYS.iter().enumerate() {
            assert_eq!(key_code(k), i as u32, "{k}");
        }
    }
}
