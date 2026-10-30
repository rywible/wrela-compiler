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
//! | `Compose` | a character of the text being composed, its index in it | the text's length in characters, 0 |
//! | `Paste` | the paste's serial number, 0 | 0, 0 |
//! | `Assist` | the node ([`ASSIST_ACTIONS`]' target), the action | 0, 0 |
//! | `Message` | the connection, the serial's low word | the serial's high word, the bytes' length |
//! | `Connection` | the connection, 1 if it's open (0: closed) | its tag (below), 0 |
//! | `Field` | the node, the field's revision | 0, 0 |
//! | `Posted` | `a`'s low and high words | `b`'s low and high words |
//!
//! A `Posted` event is never input: it's a record the program posted into its own ticks
//! (language.md's `std::tick::post`), its tag in word 1 (where an input event has its
//! modifiers) and two `u64`s, `a` and `b`, in words 2 to 5. The program asks for it with the host
//! service [`SERVICE_POST`], whose body is the event's words 1 to 5 (20 bytes) and which is never
//! answered; the host queues it for the ticker, as it queues an input event that arrives then
//! (after the call that posted it, before the next call's input), so the next tick the ticker
//! runs stamps it into its records and a tick log keeps it. A program that has started no
//! ticker fails the call's commands (a host command error). The frames never read one, and a
//! host replaying a tick log drops the program's posts: the log's records are the ticks' input.
//!
//! ## Messages
//!
//! A `Message` is bytes from a connection (language.md §17.2). Its event says how many; the
//! program copies them with `wrela.payload(serial_lo, serial_hi, ptr, cap) -> length` ([`IMPORT_PAYLOAD`]),
//! during the call that gave it the event and no later: the host keeps a message's bytes while
//! its event is queued and during that call, then lets them go. A serial is never used twice
//! by a host, so an old event's serial reads as unknown (-1), never as another message's bytes.
//! A call is given messages until their bytes reach [`PAYLOAD_PER_CALL`] (the first whatever its
//! size); the rest wait for the next call. Messages and connections never become a tick's
//! records: v1 gives the network to frames and a server's `serve`, not to ticks.
//!
//! A server program may tag a connection with a number (`std::net::tag`, a host command
//! carried out once the call's records are kept). When its host starts it again on a fresh
//! instance, the `Connection` events that tell it of its open connections carry their tags, so
//! what it knew of each connection survives the restart; a client's are 0.
//!
//! Positions are in the same pixels as `frame`'s width and height. A key is a physical key
//! (DOM's `KeyboardEvent.code`), named as [`KEYS`] lists it; one the list doesn't name is 0.
//! What a key types arrives separately, as `Text`.
//!
//! An input method (IME) composes text before it's typed: `Compose` events give the text being
//! composed, whole, each time it changes: one event a character, `index` 0 first, in order, each
//! with the text's `length` (at most [`COMPOSE_MAX`]); a length of 0 (one event, character 0)
//! means nothing is being composed. A host writes one change's events together, so a reader sees
//! all of them or none; a change whose indices don't run from 0 to `length - 1` is incomplete
//! (a reader that fell behind skipped some) and is ignored. Text composed and committed arrives
//! as `Text`, after the `Compose` that clears it.
//!
//! `Paste` says a person pasted: the program asks its host for what was pasted by the serial
//! (a host service, `clipboard/pasted`). `Assist` is an action an assistive technology or the
//! keyboard took on one of the program's semantic nodes (a host service, `semantics/tree`):
//! activate, select or focus it, step a slider's value up or down, or dismiss a dialog or a menu
//! ([`ASSIST_ACTIONS`]), by the node's number, whatever is drawn where. `Field` says a field the
//! host keeps for a node changed (its text, selection or composition): the program reads it by the
//! node (`field/state`); the revision is the field's newest, and the host says so again until the
//! program has read it.
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
//! `text` (a `Text` event for each character), `compose` (the `Compose` events for its `text`,
//! which may be empty), `paste` (its `serial`, default 0), `assist` (its `node`, and its `action`
//! as [`ASSIST_ACTIONS`] names it, default `activate`), `padbutton`, `padaxis` or `padpress` (a
//! pad's button pressed then let go); `button` defaults to `primary`; `shift`, `ctrl`, `alt` and
//! `meta` default to false. A pad's events name its `pad` (0 to 3, default 0); a `padbutton`
//! names its `button` (as [`PAD_BUTTONS`] does) and whether it's `pressed`, and its `value`
//! defaults to 1 pressed and 0 not; a `padaxis` names its `axis` ([`PAD_AXES`]) and its
//! `value`, from -1 to 1. Instead of a name, a key may be its number (`"code": 1` for
//! `"key": "KeyA"`, as [`KEYS`] numbers them), and a `button`, an `axis` or an `action` its
//! number in its list: std's `Script` writes them so (`wrela script` prints one). [`parse_script`]
//! reads one.
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

/// Whether a call already given `given` payload bytes is given a message of `len` more: the
/// first whatever its size, then up to [`PAYLOAD_PER_CALL`].
pub fn within_call(given: usize, len: usize) -> bool {
    given == 0 || given + len <= PAYLOAD_PER_CALL
}
/// Bytes per event: six words.
pub const EVENT_SIZE: u32 = 24;
/// The host service a program posts a record into its ticks with ([`EventKind::Posted`]).
pub const SERVICE_POST: &str = "tick/post";
/// The bytes of a post's body: a `Posted` event's words 1 to 5.
pub const POST_BODY: usize = 20;

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
    Compose = 10,
    Paste = 11,
    Assist = 12,
    Message = 13,
    Connection = 14,
    Field = 15,
    Posted = 16,
}

impl EventKind {
    pub const ALL: [EventKind; 16] = [
        EventKind::PointerMove,
        EventKind::PointerDown,
        EventKind::PointerUp,
        EventKind::Wheel,
        EventKind::KeyDown,
        EventKind::KeyUp,
        EventKind::Text,
        EventKind::PadButton,
        EventKind::PadAxis,
        EventKind::Compose,
        EventKind::Paste,
        EventKind::Assist,
        EventKind::Message,
        EventKind::Connection,
        EventKind::Field,
        EventKind::Posted,
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
            EventKind::Compose => "Compose",
            EventKind::Paste => "Paste",
            EventKind::Assist => "Assist",
            EventKind::Message => "Message",
            EventKind::Connection => "Connection",
            EventKind::Field => "Field",
            EventKind::Posted => "Posted",
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

/// The most characters a `Compose` change holds: a host sends no more of a longer one.
pub const COMPOSE_MAX: u32 = 64;

/// What an `Assist` does to its node (word `b`), by number.
pub const ASSIST_ACTIONS: [&str; 6] =
    ["activate", "select", "focus", "increment", "decrement", "dismiss"];

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

    /// The `Compose` events for `text`, the text being composed (at most [`COMPOSE_MAX`]
    /// characters are kept): one a character, or one of length 0 when `text` is empty.
    pub fn compose(text: &str) -> Vec<Event> {
        let chars: Vec<char> = text.chars().take(COMPOSE_MAX as usize).collect();
        let n = chars.len() as u32;
        if n == 0 {
            return vec![Event { kind: EventKind::Compose, modifiers: 0, words: [0; 4] }];
        }
        (0..n)
            .map(|i| Event {
                kind: EventKind::Compose,
                modifiers: 0,
                words: [chars[i as usize] as u32, i, n, 0],
            })
            .collect()
    }

    pub fn paste(serial: u32) -> Event {
        Event { kind: EventKind::Paste, modifiers: 0, words: [serial, 0, 0, 0] }
    }

    pub fn assist(node: u32, action: u32) -> Event {
        Event { kind: EventKind::Assist, modifiers: 0, words: [node, action, 0, 0] }
    }

    /// The field the host keeps for `node` changed: its newest revision.
    pub fn field(node: u32, revision: u32) -> Event {
        Event { kind: EventKind::Field, modifiers: 0, words: [node, revision, 0, 0] }
    }

    /// A record the program posted into its ticks: its tag, `a` and `b`.
    pub fn posted(tag: u32, a: u64, b: u64) -> Event {
        let words = [a as u32, (a >> 32) as u32, b as u32, (b >> 32) as u32];
        Event { kind: EventKind::Posted, modifiers: tag, words }
    }

    /// The record a post's body ([`SERVICE_POST`]) makes, or why it makes none.
    pub fn from_post(body: &[u8]) -> Result<Event, String> {
        if body.len() != POST_BODY {
            return Err(format!(
                "a post's body is {POST_BODY} bytes (its tag, `a` and `b`), not {}",
                body.len()
            ));
        }
        let w = |i: usize| u32::from_le_bytes(body[4 * i..4 * i + 4].try_into().expect("4 bytes"));
        Ok(Event { kind: EventKind::Posted, modifiers: w(0), words: [w(1), w(2), w(3), w(4)] })
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

    /// Connection `conn` opened, or closed; `tag` is the tag its server program gave it (0:
    /// none).
    pub fn connection(conn: u32, open: bool, tag: u32) -> Event {
        Event { kind: EventKind::Connection, modifiers: 0, words: [conn, u32::from(open), tag, 0] }
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
        // A whole number, or `default` where the key is missing.
        let whole = |key: &str, default: Option<u32>| {
            number(key)
                .unwrap_or(default)
                .ok_or_else(|| at(&format!("`{key}` must be a whole number")))
        };
        let required = |key: &str| match o.get(key) {
            Some(_) => Ok(()),
            None => Err(at(&format!("`{key}` is missing"))),
        };
        // The text a `text` or `compose` event types.
        let typed = || o.get("text").and_then(Value::as_str).ok_or_else(|| at("`text` is missing"));
        let modifiers = [("shift", SHIFT), ("ctrl", CONTROL), ("alt", ALT), ("meta", META)]
            .iter()
            .try_fold(0, |m, (k, bit)| flag(k).map(|on| if on { m | bit } else { m }))?;
        let ty = o.get("type").and_then(Value::as_str).ok_or_else(|| at("`type` is missing"))?;
        // A name of `names`, or its number.
        let index = |key: &str, names: &[&str]| -> Option<Option<u32>> {
            let v = o.get(key)?;
            let n = match v.as_str() {
                Some(name) => names.iter().position(|n| *n == name).map(|i| i as u32),
                None => number(key).flatten(),
            };
            Some(n.filter(|&n| (n as usize) < names.len()))
        };
        let button = || -> Result<u32, String> {
            match index("button", &BUTTONS) {
                None => Ok(0),
                Some(b) => {
                    b.ok_or_else(|| at("`button` is primary, middle or secondary (0, 1 or 2)"))
                }
            }
        };
        let key = || -> Result<u32, String> {
            match (o.get("key"), o.get("code")) {
                (Some(_), Some(_)) => Err(at("a key is its `key` or its `code`, not both")),
                (None, Some(_)) => number("code")
                    .flatten()
                    .filter(|&k| k != 0 && (k as usize) < KEYS.len())
                    .ok_or_else(|| {
                        at(&format!("`code` is a key's number, 1 to {}", KEYS.len() - 1))
                    }),
                (Some(v), None) => {
                    let name = v.as_str().ok_or_else(|| at("`key` must be a DOM code"))?;
                    match key_code(name) {
                        0 => Err(at(&format!(
                            "`{name}` isn't a key (they're DOM codes, such as KeyA)"
                        ))),
                        k => Ok(k),
                    }
                }
                (None, None) => Err(at("`key` (or `code`) is missing")),
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
        let named = |key: &str, names: &[&str]| -> Result<u32, String> {
            index(key, names).ok_or_else(|| at(&format!("`{key}` is missing")))?.ok_or_else(|| {
                let v = &o[key];
                let v = v.as_str().map_or_else(|| v.to_string(), str::to_string);
                at(&format!(
                    "`{v}` isn't a pad's {key} ({}, or 0 to {})",
                    names.join(", "),
                    names.len() - 1
                ))
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
                for c in typed()?.chars() {
                    push(Event::text(c));
                }
            }
            "compose" => {
                let t = typed()?;
                if t.chars().count() > COMPOSE_MAX as usize {
                    return Err(at(&format!("`text` is more than {COMPOSE_MAX} characters")));
                }
                for e in Event::compose(t) {
                    push(e);
                }
            }
            "paste" => push(Event::paste(whole("serial", Some(0))?)),
            "assist" => {
                let node = whole("node", None)?;
                let action = match index("action", &ASSIST_ACTIONS) {
                    None => 0,
                    Some(a) => a.ok_or_else(|| {
                        at("`action` is activate, select, focus, increment, decrement or dismiss (0 to 5)")
                    })?,
                };
                push(Event::assist(node, action));
            }
            "field" => push(Event::field(whole("node", None)?, whole("revision", None)?)),
            "message" | "connection" if matches!(when, At::Tick(_)) => {
                return Err(at(
                    "a tick has no network events: give a message or a connection a `frame`",
                ));
            }
            "message" => {
                let conn = whole("conn", Some(0))?;
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
                let conn = whole("conn", Some(0))?;
                required("open")?;
                push(Event::connection(conn, flag("open")?, 0));
            }
            "padbutton" => {
                let (p, b) = (pad()?, named("button", &PAD_BUTTONS)?);
                required("pressed")?;
                let pressed = flag("pressed")?;
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
                required("value")?;
                let value = num("value")?;
                if !(-1.0..=1.0).contains(&value) {
                    return Err(at("an axis's `value` is from -1 to 1"));
                }
                push(Event::pad_axis(p, a, value));
            }
            other => {
                return Err(at(&format!(
                    "`{other}` isn't a type (move, down, up, wheel, keydown, keyup, key, text, compose, paste, assist, field, padbutton, padpress, padaxis, message or connection)"
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
                if !within_call(given, len) {
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

    /// The build changed (a reload to another pair, language.md §17.2): queued messages and
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

    /// A post's body is a `Posted` record's words 1 to 5: its tag, then `a` and `b`, each low
    /// word first; a body of another length makes none.
    #[test]
    fn a_posts_body_is_its_records_words() {
        let (a, b) = (0x0102_0304_0506_0708u64, 0xfedc_ba98_7654_3210u64);
        let mut body = 7u32.to_le_bytes().to_vec();
        body.extend(a.to_le_bytes());
        body.extend(b.to_le_bytes());
        let e = Event::from_post(&body).expect("a record");
        assert_eq!(e, Event::posted(7, a, b));
        assert_eq!(e.bytes()[..4], (EventKind::Posted as u32).to_le_bytes());
        assert_eq!(e.bytes()[4..], body[..]);
        assert!(!e.kind.is_network());
        assert!(Event::from_post(&body[..19]).unwrap_err().contains("20 bytes"));
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
        q.push(Event::connection(0, true, 0));
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
        assert_eq!(s[0].event, Event::connection(2, true, 0));
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

    /// Composing, pasting and assistive actions, as a script gives them.
    #[test]
    fn compose_paste_and_assist_read_back() {
        let script = r#"[
            {"frame": 0, "type": "compose", "text": "かな"},
            {"frame": 1, "type": "compose", "text": ""},
            {"frame": 1, "type": "text", "text": "仮名"},
            {"frame": 2, "type": "paste", "serial": 7},
            {"frame": 3, "type": "assist", "node": 1004, "action": "select"},
            {"frame": 3, "type": "assist", "node": 2}
        ]"#;
        let s = parse_script(script).expect("a valid script");
        let words: Vec<_> = s.iter().map(|e| (e.event.kind, e.event.words)).collect();
        assert_eq!(words[0], (EventKind::Compose, ['か' as u32, 0, 2, 0]));
        assert_eq!(words[1], (EventKind::Compose, ['な' as u32, 1, 2, 0]));
        assert_eq!(words[2], (EventKind::Compose, [0, 0, 0, 0]), "an empty one clears");
        assert_eq!(words[5], (EventKind::Paste, [7, 0, 0, 0]));
        assert_eq!(words[6], (EventKind::Assist, [1004, 1, 0, 0]));
        assert_eq!(words[7], (EventKind::Assist, [2, 0, 0, 0]), "activate by default");
        let long = "x".repeat(COMPOSE_MAX as usize + 1);
        assert_eq!(Event::compose(&long).len(), COMPOSE_MAX as usize, "a long one is cut");
        for (text, why) in [
            (format!(r#"[{{"frame": 0, "type": "compose", "text": "{long}"}}]"#), "more than"),
            (r#"[{"frame": 0, "type": "assist"}]"#.to_string(), "`node` must be"),
            (
                r#"[{"frame": 0, "type": "assist", "node": 1, "action": "poke"}]"#.to_string(),
                "`action` is",
            ),
            (r#"[{"frame": 0, "type": "paste", "serial": -1}]"#.to_string(), "`serial` must be"),
        ] {
            let e = parse_script(&text).expect_err(&text);
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

    /// A key's number (`code`) and a button's, an axis's or an action's number read as its name
    /// does; a number past its list says why.
    #[test]
    fn numbers_read_as_names() {
        let named = r#"[
            {"frame": 0, "type": "key", "key": "KeyA", "meta": true},
            {"frame": 1, "type": "down", "x": 1, "y": 2, "button": "secondary"},
            {"frame": 2, "type": "assist", "node": 4, "action": "dismiss"},
            {"frame": 3, "type": "padbutton", "button": "righttrigger", "pressed": true},
            {"frame": 3, "type": "padaxis", "axis": "righty", "value": 0.25}
        ]"#;
        let numbered = r#"[
            {"frame": 0, "type": "key", "code": 1, "meta": true},
            {"frame": 1, "type": "down", "x": 1, "y": 2, "button": 2},
            {"frame": 2, "type": "assist", "node": 4, "action": 5.0},
            {"frame": 3, "type": "padbutton", "button": 7, "pressed": true},
            {"frame": 3, "type": "padaxis", "axis": 3, "value": 0.25}
        ]"#;
        assert_eq!(parse_script(named), parse_script(numbered));
        for (text, why) in [
            (r#"[{"frame": 0, "type": "key", "code": 0}]"#, "`code` is a key's number, 1 to 89"),
            (r#"[{"frame": 0, "type": "key", "code": 90}]"#, "`code` is a key's number"),
            (r#"[{"frame": 0, "type": "key", "code": 1.5}]"#, "`code` is a key's number"),
            (r#"[{"frame": 0, "type": "key", "key": "KeyA", "code": 1}]"#, "not both"),
            (r#"[{"frame": 0, "type": "key"}]"#, "`key` (or `code`) is missing"),
            (r#"[{"frame": 0, "type": "down", "button": 3}]"#, "`button` is primary"),
            (r#"[{"frame": 0, "type": "assist", "node": 1, "action": 6}]"#, "`action` is"),
            (r#"[{"frame": 0, "type": "padpress", "button": 17}]"#, "`17` isn't a pad's button"),
            (r#"[{"frame": 0, "type": "padaxis", "axis": -1, "value": 0}]"#, "isn't a pad's axis"),
        ] {
            let e = parse_script(text).expect_err(text);
            assert!(e.contains(why), "{text}: {e}");
        }
    }

    /// std's `Script` writes keys, pointer buttons, pad buttons and axes, and assistive actions
    /// as their std enums' discriminants (compiler/std/input.wrela), which are their numbers
    /// here: each enum's variants are this module's list, in order.
    #[test]
    fn std_enums_number_as_the_abi_does() {
        let std = include_str!("../../../compiler/std/input.wrela");
        let variants = |name: &str| -> Vec<String> {
            let head = format!("pub enum {name}:");
            let start = std.find(&head).unwrap_or_else(|| panic!("std has no enum {name}"));
            std[start..]
                .lines()
                .skip(1)
                .map(str::trim)
                .take_while(|l| *l != "}")
                .filter(|l| !l.is_empty() && !l.starts_with("//"))
                .map(|l| l.strip_suffix(',').expect("a fieldless variant").to_string())
                .collect()
        };
        let lower = |name: &str| -> Vec<String> {
            variants(name).iter().map(|v| v.to_lowercase()).collect()
        };
        assert_eq!(lower("Button"), BUTTONS);
        assert_eq!(lower("PadButton"), PAD_BUTTONS);
        assert_eq!(lower("PadAxis"), PAD_AXES);
        assert_eq!(lower("Assist"), ASSIST_ACTIONS);
        // std's keys drop DOM's `Key` and `Arrow`.
        let keys = variants("Key");
        assert_eq!(keys.len(), KEYS.len());
        for (i, (k, dom)) in keys.iter().zip(KEYS).enumerate() {
            let same = *k == dom || format!("Key{k}") == dom || format!("Arrow{k}") == dom;
            assert!(same, "std's Key::{k} is key {i}, which is {dom}");
            // `key_of`, which reads a key's number, agrees.
            if i > 0 {
                assert!(std.contains(&format!("        {i} => Key::{k},\n")), "key_of({i})");
            }
        }
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
