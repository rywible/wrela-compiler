//! # Input
//!
//! A program reads pointer, keyboard and gamepad events through the import
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
//! | `PadButton` | the pad (0 to 3), the button ([`PAD_BUTTONS`]) | its value (f32, 0 to 1), 1 if it's pressed |
//! | `PadAxis` | the pad, the axis ([`PAD_AXES`]) | its value (f32, -1 to 1), 0 |
//! | `Message` | the connection, the serial's low word | the serial's high word, the bytes' length |
//! | `Connection` | the connection, 1 if it's open (0: closed) | 0, 0 |
//!
//! ## Messages
//!
//! A `Message` is bytes from a connection (language.md §12). Its event says how many; the
//! program copies them with `wrela.payload(serial_lo, serial_hi, ptr, cap) -> length` ([`IMPORT_PAYLOAD`]),
//! during the call that gave it the event and no later: the host keeps a message's bytes while
//! its event is queued and during that call, then lets them go. A serial is never used twice
//! by a host, so an old event's serial reads as unknown (-1), never as another message's bytes.
//! A call is given messages until their bytes reach [`PAYLOAD_PER_CALL`] (the first whatever its
//! size); the rest wait for the next call. Messages and connections never become a tick's
//! records: v1 gives the network to frames and a server's `serve`, not to ticks.
//!
//! Positions are in the same pixels as `frame`'s width and height. A key is a physical key
//! (DOM's `KeyboardEvent.code`), named as [`KEYS`] lists it; one the list doesn't name is 0.
//! What a key types arrives separately, as `Text`.
//!
//! A gamepad's buttons and axes are the W3C Gamepad API's standard mapping, by number. A host
//! sends an event when a button's value or pressed state changes, or an axis's value: a pad
//! held still sends nothing. A stick's axes are as the pad gives them (right and down are
//! positive); no dead zone is applied, which is the program's choice.
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
//! `text` (a `Text` event for each character), `padbutton`, `padaxis` or `padpress` (a pad's
//! button pressed then let go); `button` defaults to `primary`; `shift`, `ctrl`, `alt` and
//! `meta` default to false. A pad's events name its `pad` (0 to 3, default 0); a `padbutton`
//! names its `button` (as [`PAD_BUTTONS`] does) and whether it's `pressed`, and its `value`
//! defaults to 1 pressed and 0 not; a `padaxis` names its `axis` ([`PAD_AXES`]) and its
//! `value`, from -1 to 1. [`parse_script`] reads one.
//!
//! ```json
//! [
//!   { "tick": 30, "type": "padaxis", "axis": "lefty", "value": -1 },
//!   { "tick": 40, "type": "padpress", "button": "east" },
//!   { "tick": 50, "type": "padbutton", "button": "righttrigger", "pressed": true, "value": 0.8 }
//! ]
//! ```
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
/// `wrela.payload(serial_lo, serial_hi, ptr, cap) -> length`: a message's bytes ([Messages](self#messages)).
pub const IMPORT_PAYLOAD: &str = "payload";
/// The payload bytes one call is given at most, past its first message.
pub const PAYLOAD_PER_CALL: usize = 1 << 20;
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
    PadButton = 8,
    PadAxis = 9,
    // 10 to 12 are the platform's (an input method's text, a paste, an assistive action).
    Message = 13,
    Connection = 14,
}

impl EventKind {
    pub const ALL: [EventKind; 11] = [
        EventKind::PointerMove,
        EventKind::PointerDown,
        EventKind::PointerUp,
        EventKind::Wheel,
        EventKind::KeyDown,
        EventKind::KeyUp,
        EventKind::Text,
        EventKind::PadButton,
        EventKind::PadAxis,
        EventKind::Message,
        EventKind::Connection,
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
            EventKind::PadButton => "PadButton",
            EventKind::PadAxis => "PadAxis",
            EventKind::Message => "Message",
            EventKind::Connection => "Connection",
        }
    }

    /// Whether it's the network's: never a tick's record.
    pub fn is_network(self) -> bool {
        matches!(self, EventKind::Message | EventKind::Connection)
    }
}

/// The modifier bits (word 1).
pub const SHIFT: u32 = 1;
pub const CONTROL: u32 = 2;
pub const ALT: u32 = 4;
pub const META: u32 = 8;

/// The buttons (`c` of a pointer down or up).
pub const BUTTONS: [&str; 3] = ["primary", "middle", "secondary"];

/// How many gamepads a host reports: pads 0 to 3.
pub const PADS: u32 = 4;

/// A gamepad's buttons, by number: the W3C Gamepad API's standard mapping (button `i` of a
/// pad whose `mapping` is "standard"). Named by where they are, not by what a pad prints on
/// them: `south` is the bottom face button (A on an Xbox pad, Cross on a PlayStation one).
pub const PAD_BUTTONS: [&str; 17] = [
    "south",
    "east",
    "west",
    "north",
    "leftshoulder",
    "rightshoulder",
    "lefttrigger",
    "righttrigger",
    "select",
    "start",
    "leftstick",
    "rightstick",
    "up",
    "down",
    "left",
    "right",
    "home",
];

/// A gamepad's axes, by number, as the standard mapping has them: each stick's x (right
/// positive) and y (down positive).
pub const PAD_AXES: [&str; 4] = ["leftx", "lefty", "rightx", "righty"];

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

    pub fn pad_button(pad: u32, button: u32, value: f32, pressed: bool) -> Event {
        let words = [pad, button, value.to_bits(), u32::from(pressed)];
        Event { kind: EventKind::PadButton, modifiers: 0, words }
    }

    pub fn pad_axis(pad: u32, axis: u32, value: f32) -> Event {
        Event { kind: EventKind::PadAxis, modifiers: 0, words: [pad, axis, value.to_bits(), 0] }
    }

    /// A message of `len` bytes from connection `conn`, its serial `serial`.
    pub fn message(conn: u32, serial: u64, len: u32) -> Event {
        let words = [conn, serial as u32, (serial >> 32) as u32, len];
        Event { kind: EventKind::Message, modifiers: 0, words }
    }

    /// Connection `conn` opened, or closed.
    pub fn connection(conn: u32, open: bool) -> Event {
        Event { kind: EventKind::Connection, modifiers: 0, words: [conn, u32::from(open), 0, 0] }
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

/// A script's event, and when it arrives; a message's bytes (its serial is the host's to give,
/// when it's queued).
#[derive(Clone, Debug, PartialEq)]
pub struct Scripted {
    pub at: At,
    pub event: Event,
    pub payload: Vec<u8>,
}

/// The events of `script` that arrive before frame `frame`, for [`Queue::push_scripted`].
pub fn events_at(script: &[Scripted], frame: u32) -> impl Iterator<Item = &Scripted> + '_ {
    script.iter().filter(move |s| s.at == At::Frame(frame))
}

/// The events of `script` that are tick `tick`'s records. A tick has no network events.
pub fn records_at(script: &[Scripted], tick: u32) -> impl Iterator<Item = Event> + '_ {
    script
        .iter()
        .filter(move |s| s.at == At::Tick(tick) && !s.event.kind.is_network())
        .map(|s| s.event)
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
            o.get(key).map(|v| {
                v.as_f64()
                    .filter(|f| f.is_finite() && f.fract() == 0.0 && *f >= 0.0)
                    .filter(|&f| f <= f64::from(u32::MAX))
                    .map(|f| f as u32)
            })
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
        let pad = || -> Result<u32, String> {
            match o.get("pad") {
                None => Ok(0),
                Some(_) => number("pad")
                    .flatten()
                    .filter(|&p| p < PADS)
                    .ok_or_else(|| at("`pad` is 0, 1, 2 or 3")),
            }
        };
        let named =
            |key: &str, names: &[&str]| -> Result<u32, String> {
                let name = o
                    .get(key)
                    .and_then(Value::as_str)
                    .ok_or_else(|| at(&format!("`{key}` is missing")))?;
                names.iter().position(|n| *n == name).map(|i| i as u32).ok_or_else(|| {
                    at(&format!("`{name}` isn't a pad's {key} ({})", names.join(", ")))
                })
            };
        let mut push = |event| out.push(Scripted { at: when, event, payload: Vec::new() });
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
            "message" | "connection" if matches!(when, At::Tick(_)) => {
                return Err(at(
                    "a tick has no network events: give a message or a connection a `frame`",
                ));
            }
            "message" => {
                let conn = number("conn")
                    .unwrap_or(Some(0))
                    .ok_or_else(|| at("`conn` must be a whole number"))?;
                let payload = match (o.get("bytes"), o.get("text")) {
                    (Some(b), None) => b
                        .as_array()
                        .and_then(|a| {
                            a.iter()
                                // As every number here: `1.0` is 1.
                                .map(|x| {
                                    x.as_f64()
                                        .filter(|f| f.fract() == 0.0 && (0.0..256.0).contains(f))
                                        .map(|f| f as u8)
                                })
                                .collect::<Option<Vec<u8>>>()
                        })
                        .ok_or_else(|| at("`bytes` is an array of numbers from 0 to 255"))?,
                    (None, Some(t)) => {
                        t.as_str().ok_or_else(|| at("`text` must be a string"))?.as_bytes().to_vec()
                    }
                    _ => return Err(at("a message has `bytes` or `text`")),
                };
                let len = payload.len() as u32;
                out.push(Scripted { at: when, event: Event::message(conn, 0, len), payload });
            }
            "connection" => {
                let conn = number("conn")
                    .unwrap_or(Some(0))
                    .ok_or_else(|| at("`conn` must be a whole number"))?;
                let open = match o.get("open") {
                    Some(v) => v.as_bool().ok_or_else(|| at("`open` must be true or false"))?,
                    None => return Err(at("`open` is missing")),
                };
                push(Event::connection(conn, open));
            }
            "padbutton" => {
                let (p, b) = (pad()?, named("button", &PAD_BUTTONS)?);
                let pressed = match o.get("pressed") {
                    Some(v) => v.as_bool().ok_or_else(|| at("`pressed` must be true or false"))?,
                    None => return Err(at("`pressed` is missing")),
                };
                let value = match o.get("value") {
                    None => f32::from(u8::from(pressed)),
                    Some(_) => num("value")?,
                };
                if !(0.0..=1.0).contains(&value) {
                    return Err(at("a button's `value` is from 0 to 1"));
                }
                push(Event::pad_button(p, b, value, pressed));
            }
            "padpress" => {
                let (p, b) = (pad()?, named("button", &PAD_BUTTONS)?);
                push(Event::pad_button(p, b, 1.0, true));
                push(Event::pad_button(p, b, 0.0, false));
            }
            "padaxis" => {
                let (p, a) = (pad()?, named("axis", &PAD_AXES)?);
                if o.get("value").is_none() {
                    return Err(at("`value` is missing"));
                }
                let value = num("value")?;
                if !(-1.0..=1.0).contains(&value) {
                    return Err(at("an axis's `value` is from -1 to 1"));
                }
                push(Event::pad_axis(p, a, value));
            }
            other => {
                return Err(at(&format!(
                    "`{other}` isn't a type (move, down, up, wheel, keydown, keyup, key, text, padbutton, padpress, padaxis, message or connection)"
                )));
            }
        }
    }
    Ok(out)
}

/// A host's queue of events not yet read: [`Queue::take`] gives `wrela.input` its events, and
/// [`Queue::payload`] gives `wrela.payload` the bytes of the messages the current call was
/// given ([Messages](self#messages)).
#[derive(Clone, Debug, Default)]
pub struct Queue {
    events: std::collections::VecDeque<Event>,
    /// Each message's bytes, by serial: while its event is queued, and during the call given it.
    payloads: std::collections::HashMap<u64, Vec<u8>>,
    /// The serials of the messages the current call was given.
    delivered: Vec<u64>,
    /// The last serial given; the first is 1.
    serial: u64,
    /// The pair id of the build it serves ([`crate::read_pair`]).
    pair: Option<u64>,
}

impl Queue {
    /// Queues an event that isn't a message (a message has bytes: [`Queue::push_message`]).
    pub fn push(&mut self, e: Event) {
        debug_assert!(e.kind != EventKind::Message, "a message is queued with its bytes");
        self.events.push_back(e);
    }

    /// Queues a message of `bytes` from connection `conn`: its serial.
    pub fn push_message(&mut self, conn: u32, bytes: Vec<u8>) -> u64 {
        self.serial += 1;
        let serial = self.serial;
        self.events.push_back(Event::message(conn, serial, bytes.len() as u32));
        self.payloads.insert(serial, bytes);
        serial
    }

    /// Queues a script's event: a message with its bytes.
    pub fn push_scripted(&mut self, s: &Scripted) {
        if s.event.kind == EventKind::Message {
            self.push_message(s.event.words[0], s.payload.clone());
        } else {
            self.push(s.event);
        }
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Up to `cap` events, oldest first, as bytes; the rest stay queued. Messages stop once the
    /// call's bytes reach [`PAYLOAD_PER_CALL`] (the first is given whatever its size).
    pub fn take(&mut self, cap: u32) -> Vec<u8> {
        let mut given: usize =
            self.delivered.iter().map(|s| self.payloads.get(s).map_or(0, Vec::len)).sum();
        let mut out = Vec::new();
        while out.len() / (EVENT_SIZE as usize) < (cap as usize) {
            let Some(&e) = self.events.front() else { break };
            if e.kind == EventKind::Message {
                let len = e.words[3] as usize;
                if given > 0 && given + len > PAYLOAD_PER_CALL {
                    break;
                }
                given += len;
                self.delivered.push(u64::from(e.words[1]) | (u64::from(e.words[2]) << 32));
            }
            self.events.pop_front();
            out.extend_from_slice(&e.bytes());
        }
        out
    }

    /// The bytes of message `serial`, if the current call was given it.
    pub fn payload(&self, serial: u64) -> Option<&[u8]> {
        self.delivered.contains(&serial).then(|| self.payloads.get(&serial).map(Vec::as_slice))?
    }

    /// The call has returned (or trapped): the bytes of the messages it was given go.
    pub fn end_call(&mut self) {
        for serial in self.delivered.drain(..) {
            self.payloads.remove(&serial);
        }
    }

    /// It now serves the build whose pair id is `pair`: if that's another program's build than
    /// the one it served (a reload that changed it), its network events go ([`Queue::drop_network`]).
    pub fn for_build(&mut self, pair: Option<u64>) {
        if self.pair.is_some() && self.pair != pair {
            self.drop_network();
        }
        self.pair = pair;
    }

    /// The build changed (a reload to another pair, language.md §12): queued messages and
    /// connection events go, as their bytes were for the old build's types; the rest stay.
    pub fn drop_network(&mut self) {
        self.events.retain(|e| !e.kind.is_network());
        self.end_call();
        self.payloads.clear();
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

    /// A message's bytes are the call's that was given it, and no other's; a call stops taking
    /// messages at its bytes' limit; a serial is never answered twice.
    #[test]
    fn messages_bytes_live_for_the_call_that_reads_them() {
        let mut q = Queue::default();
        let first = q.push_message(3, vec![1, 2, 3]);
        q.push(Event::text('x'));
        let big = q.push_message(4, vec![9; PAYLOAD_PER_CALL]);
        assert_ne!(first, big);
        // Queued, not yet given: unknown.
        assert_eq!(q.payload(first), None);
        // The first call takes the small message and the text, and stops before the big one
        // (its bytes would pass the limit).
        let bytes = q.take(16);
        assert_eq!(bytes.len(), 2 * EVENT_SIZE as usize);
        assert_eq!(&bytes[..4], &(EventKind::Message as u32).to_le_bytes());
        assert_eq!(q.payload(first), Some(&[1u8, 2, 3][..]));
        assert_eq!(q.payload(big), None);
        q.end_call();
        // The next call: the first message's bytes are gone; the big one is given, whole.
        assert_eq!(q.payload(first), None);
        assert_eq!(q.take(16).len(), EVENT_SIZE as usize);
        assert_eq!(q.payload(big).map(<[u8]>::len), Some(PAYLOAD_PER_CALL));
        q.end_call();
        assert!(q.is_empty() && q.payloads.is_empty());
        // A new message never reuses a serial.
        assert!(q.push_message(3, vec![]) > big);
    }

    /// A reload to another program's build drops the network's events, and keeps the rest.
    #[test]
    fn another_builds_queue_drops_the_network() {
        let mut q = Queue::default();
        q.for_build(Some(1));
        q.push_message(0, vec![5]);
        q.push(Event::connection(0, true));
        q.push(Event::text('a'));
        // The same build, reloaded: everything stays.
        q.for_build(Some(1));
        assert_eq!(q.len(), 3);
        // Another build: only the text stays.
        q.for_build(Some(2));
        assert_eq!(q.len(), 1);
        assert!(q.payloads.is_empty());
        assert_eq!(&q.take(4)[..4], &(EventKind::Text as u32).to_le_bytes());
    }

    #[test]
    fn scripts_give_messages_and_connections_to_frames_only() {
        let script = r#"[
            {"frame": 1, "type": "connection", "conn": 2, "open": true},
            {"frame": 1, "type": "message", "conn": 2, "bytes": [1, 255]},
            {"frame": 2, "type": "message", "text": "hi"},
            {"tick": 3, "type": "key", "key": "KeyA"}
        ]"#;
        let s = parse_script(script).expect("a valid script");
        assert_eq!(s[0].event, Event::connection(2, true));
        assert_eq!(s[1].event.kind, EventKind::Message);
        assert_eq!(s[1].payload, [1, 255]);
        assert_eq!(s[2].payload, b"hi");
        let mut q = Queue::default();
        for e in events_at(&s, 1) {
            q.push_scripted(e);
        }
        assert_eq!(q.len(), 2);
        // A tick takes none.
        let bad = r#"[{"tick": 0, "type": "message", "text": "x"}]"#;
        assert!(parse_script(bad).expect_err("a tick's message").contains("a tick has no network"));
        assert_eq!(records_at(&s, 3).count(), 2);
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
            (
                r#"[{"frame": 0, "type": "padbutton", "button": "a", "pressed": true}]"#,
                "isn't a pad's",
            ),
            (r#"[{"frame": 0, "type": "padbutton", "button": "south"}]"#, "`pressed` is missing"),
            (r#"[{"frame": 0, "type": "padaxis", "axis": "leftx", "value": 2}]"#, "from -1 to 1"),
            (r#"[{"frame": 0, "type": "padaxis", "axis": "leftx"}]"#, "`value` is missing"),
            (r#"[{"frame": 0, "type": "padpress", "button": "east", "pad": 4}]"#, "`pad` is 0"),
            (r#"[{"frame": 0, "type": "key", "key": "A"}]"#, "isn't a key"),
            (r#"[{"frame": 0, "type": "down", "button": "left"}]"#, "`button` is primary"),
        ];
        for (text, why) in bad {
            let e = parse_script(text).expect_err(text);
            assert!(e.contains(why), "{text}: {e}");
        }
    }

    #[test]
    fn whole_json_numbers_neednt_use_integer_syntax() {
        for number in ["1.0", "1e0"] {
            let script = format!(r#"[{{"frame": {number}, "type": "text", "text": "x"}}]"#);
            let events = parse_script(&script).expect("a whole JSON number");
            assert_eq!(events[0].at, At::Frame(1));
        }
    }

    #[test]
    fn pad_numbers_neednt_use_integer_syntax() {
        for number in ["1", "1.0", "1e0"] {
            let script = format!(
                r#"[{{"tick": 0, "type": "padpress", "button": "east", "pad": {number}}}]"#
            );
            let events = parse_script(&script).expect("a whole pad number");
            assert_eq!(events[0].event, Event::pad_button(1, 1, 1.0, true));
        }
        for number in ["-1", "1.5", "4", "4294967297", "null", "true", "\"1\""] {
            let script = format!(
                r#"[{{"tick": 0, "type": "padpress", "button": "east", "pad": {number}}}]"#
            );
            assert_eq!(
                parse_script(&script).expect_err("invalid pad"),
                "event 0: `pad` is 0, 1, 2 or 3"
            );
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

    /// A pad's buttons and axes read back by number, with their values.
    #[test]
    fn pad_events_read_back() {
        let script = r#"[
            {"tick": 1, "type": "padaxis", "axis": "lefty", "value": -0.75},
            {"tick": 2, "type": "padbutton", "pad": 2, "button": "righttrigger", "pressed": true, "value": 0.5},
            {"tick": 3, "type": "padpress", "button": "east"},
            {"tick": 4, "type": "padbutton", "button": "south", "pressed": false}
        ]"#;
        let s = parse_script(script).expect("a valid script");
        assert_eq!(s.len(), 5);
        assert_eq!(s[0].event, Event::pad_axis(0, 1, -0.75));
        assert_eq!(s[1].event, Event::pad_button(2, 7, 0.5, true));
        assert_eq!(s[2].event, Event::pad_button(0, 1, 1.0, true));
        assert_eq!(s[3].event, Event::pad_button(0, 1, 0.0, false));
        assert_eq!(s[4].event.words, [0, 0, 0, 0]);
        assert_eq!(&s[1].event.bytes()[..4], &8u32.to_le_bytes());
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
