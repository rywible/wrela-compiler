//! Focused performance regressions using the engine and UI's actual source in small packages.
//! Reference code keeps the old operations to compare ordering and every float's bits.

use std::path::PathBuf;

fn source(path: &str) -> String {
    std::fs::read_to_string(wrela_tests::repo_root().join(path)).expect("read source")
}

fn package(name: &str, main: &str) -> PathBuf {
    let dir =
        wrela_tests::scratch_root(env!("CARGO_TARGET_TMPDIR")).join(format!("engine-perf-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch package");
    std::fs::write(dir.join("main.wrela"), main).expect("write main");
    dir
}

fn passes(dir: &std::path::Path, count: usize) {
    let out = wrela_driver::test(dir, None);
    assert!(
        out.passed(),
        "{}\n{:#?}",
        wrela_diag::render::render_all(&out.sources, &out.diagnostics),
        out.results
    );
    assert_eq!(out.results.len(), count);
}

/// Copies one complete function, retaining its body from production rather than a test copy.
fn function(text: &str, declaration: &str) -> String {
    let start = text.find(declaration).expect("function declaration");
    let body = start + text[start..].find('{').expect("function body");
    let mut depth = 0;
    for (i, b) in text[body..].bytes().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return text[start..=body + i].to_owned();
                }
            }
            _ => {}
        }
    }
    panic!("unclosed function");
}

#[test]
fn perf_stream_smoothing_keeps_every_bit_without_allocating() {
    let smooth = function(&source("engine/grow.wrela"), "fn smooth_course(");
    let tests = r#"
use std::alloc::allocations

fn reference(points: mut Vec<vec4>) {
    let was = points.clone()
    for k in 1..(max(was.len(), 2) - 1) {
        let mid = (was[k - 1].xy + was[k + 1].xy) * 0.5
        points[k] = vec4(mix(was[k].xy, mid, vec2(0.5)), was[k].z, was[k].w)
    }
}

@test
fn every_length_and_both_passes() {
    for n in 0..65 {
        for path in 0..6 {
            var points = Vec::from_fn(n, |k| {
                let x = if path == 0 { f32(k) * 0.125 } else if path == 1 {
                    if k % 2 == 0 { -1000.0 } else { 1000.0 }
                } else if path == 2 { bitcast_f32(if k % 2 == 0 { u32(0) } else { u32(2147483648) })
                } else if path == 3 { bitcast_f32(k + 1)
                } else if path == 4 { 1.0e30 / f32(k + 1)
                } else { f32((k * 7919 + 104729) % 1237) - 600.0 }
                vec4(x, x * -0.75, f32(k) + 0.25, f32(k) * -0.375)
            })
            var expected = points.clone()
            let before = allocations()
            smooth_course(mut points)
            smooth_course(mut points)
            assert(allocations() == before, "smoothing allocated")
            reference(mut expected)
            reference(mut expected)
            for k in 0..n {
                let (a, b) = (points[k], expected[k])
                assert(bitcast_u32(a.x) == bitcast_u32(b.x), "x changed")
                assert(bitcast_u32(a.y) == bitcast_u32(b.y), "y changed")
                assert(bitcast_u32(a.z) == bitcast_u32(b.z), "width changed")
                assert(bitcast_u32(a.w) == bitcast_u32(b.w), "depth changed")
            }
        }
    }
}

pub fn frame(time: f32, width: u32, height: u32) {}
"#;
    let dir = package("stream", &format!("{smooth}\n{tests}"));
    passes(&dir, 1);
}

#[test]
fn perf_timer_search_preserves_due_ticks_sequence_and_cancellation() {
    // Leave the methods untouched; their unrelated sim derives become Clone in this isolated
    // package, which lets the test inspect the private pending list without all the engine.
    let timers = source("engine/timers.wrela")
        .replace("use sim::{Sim, Tick}", "type Tick = u32")
        .replace(": Sim {", ": Clone {");
    let tests = r#"
fn reference(pending: mut Vec<Timer<u32>>, now: Tick, delay: f32, dt: f32, seq: u64) {
    let due = now + u32(ceil(delay / dt))
    var at = pending.len()
    while at > 0 && pending[at - 1].due > due {
        at -= 1
    }
    pending.insert(at, Timer { due, seq, action: u32(seq) })
}

fn same(timers: Timers<u32>, expected: [Timer<u32>]) {
    assert(timers.len() == expected.len(), "timer count changed")
    for k in 0..expected.len() {
        borrow a = timers.pending[k]
        borrow b = expected[k]
        assert(a.due == b.due && a.seq == b.seq && a.action == b.action, "timer order changed")
    }
}

@test
fn insertion_and_draining() {
    for pattern in 0..5 {
        var timers = Timers::<u32>::new()
        var expected: Vec<Timer<u32>> = Vec::new()
        let now = if pattern == 4 { 4000000000 } else { 12 }
        for i in 0..256 {
            let delay = if pattern == 0 { f32(i) } else if pattern == 1 { f32(255 - i)
            } else if pattern == 2 { 3.25 } else { f32((i * 37) % 101) * 0.125 }
            let dt = if pattern == 3 { 0.125 } else { 0.5 }
            let seq = timers.after(now, delay, dt, action: i)
            assert(seq == u64(i), "timer id changed")
            reference(mut expected, now, delay, dt, seq)
            same(timers, expected)
            if i % 7 == 3 {
                let canceled = u64(i / 2)
                timers.cancel(canceled)
                expected.retain(|t| t.seq != canceled)
                same(timers, expected)
            }
        }
        for tick in 0..514 {
            var n = 0
            while n < expected.len() && expected[n].due <= now + tick {
                n += 1
            }
            let want = expected.drain_front(n)
            let got = timers.take_due(now + tick)
            assert(got.len() == want.len(), "due count changed")
            for k in 0..want.len() {
                assert(got[k].due == want[k].due && got[k].seq == want[k].seq &&
                    got[k].action == want[k].action, "due order changed")
            }
            same(timers, expected)
        }
        assert(timers.len() == 0, "timers left pending")
    }
}

pub fn frame(time: f32, width: u32, height: u32) {}
"#;
    let dir = package("timers", &format!("{timers}\n{tests}"));
    passes(&dir, 1);
}

#[test]
fn perf_ui_frame_buffers_and_typing_preserve_behavior() {
    let dir = package("ui", "pub fn frame(time: f32, width: u32, height: u32) {}\n");
    for file in ["widgets.wrela", "draw.wrela", "text.wrela", "ttf.wrela"] {
        std::fs::write(dir.join(file), source(&format!("ui/{file}"))).expect("copy UI source");
    }
    // A zero-area fallback glyph lays out text without a bake or draw; its self-kern also
    // exercises the caret regression when both letters fall back to it.
    let empty = "Baked { em: 32.0, spread: 4.0, ascender: 0.0, descender: 0.0, line_gap: 0.0, glyphs: Vec::from([ttf::BakedGlyph { codepoint: 63, advance: 1.0, left: 0.0, top: 0.0, width: 0, height: 0, offset: 0 }]), kerning: Vec::from([ttf::Kern { left: 0, right: 0, units: -4096 }]), fields: Vec::new() }";
    let text = source("ui/text.wrela")
        .replace("bake(embed(\"fonts/Inter-Regular-Latin1.ttf\"), em: 32.0, spread: 4.0)", empty)
        .replace(
            "bake(embed(\"fonts/JetBrainsMono-Regular-Latin1.ttf\"), em: 32.0, spread: 4.0)",
            empty,
        );
    std::fs::write(dir.join("text.wrela"), text).expect("empty test fonts");
    let tests = r#"
impl Ui {
    // The previous edit_keys body, to compare text, cursor, focus and termination exactly.
    fn reference_edit_keys(mut self, id: u32, s: mut String) -> u32 {
        var cursor = min(self.cursor(), s.len())
        var done = 0
        var t = 0
        for j in 0..self.keys.len() + 1 {
            let upto = if j < self.keys.len() { self.typed_before[j] } else { self.typed.len() }
            while t < upto {
                let tail = s.as_str().slice(cursor, s.len()).to_string()
                s.truncate(cursor)
                s.push_char(self.typed[t])
                cursor = s.len()
                s.push_str(tail.as_str())
                t += 1
            }
            if j == self.keys.len() { break }
            match self.keys[j] {
                Key::Backspace => {
                    if cursor > 0 {
                        let from = previous(s.as_str(), cursor)
                        let tail = s.as_str().slice(cursor, s.len()).to_string()
                        s.truncate(from)
                        s.push_str(tail.as_str())
                        cursor = from
                    }
                },
                Key::Delete => {
                    if cursor < s.len() {
                        let (_, next) = decode(s.as_str(), cursor)
                        let tail = s.as_str().slice(next, s.len()).to_string()
                        s.truncate(cursor)
                        s.push_str(tail.as_str())
                    }
                },
                Key::Left => cursor = previous(s.as_str(), cursor),
                Key::Right => {
                    if cursor < s.len() { cursor = decode(s.as_str(), cursor).1 }
                },
                Key::Home => cursor = 0,
                Key::End => cursor = s.len(),
                Key::Enter => done = 1,
                Key::NumpadEnter => done = 1,
                Key::Escape => done = 2,
                _ => {},
            }
        }
        self.set_cursor(cursor)
        if done != 0 {
            self.editing = None
            self.focus = if done == 1 { id } else { 0 }
        }
        done
    }
}

@test
fn warm_frames_do_not_allocate() {
    var ui = Ui::new(Fonts::load())
    let screen = vec2(640.0, 480.0)
    let r = Rect::new(x: 12.0, y: 24.0, w: 80.0, h: 32.0)
    // Both buffers get room, then every frame keeps its predecessor and empties its own.
    for frame in 0..4 {
        ui.begin(screen, [Event::Text { char: 0 }; 0])
        ui.note("seen", r)
        for id in 1..33 {
            ui.focusable.push(id)
        }
        ui.end()
    }
    let before = std::alloc::allocations()
    for frame in 0..12 {
        ui.begin(screen, [Event::Text { char: 0 }; 0])
        assert(ui.seen.is_empty() && ui.focusable.is_empty(), "frame was not cleared")
        assert(ui.find("seen") == Some(r), "previous frame label lost")
        assert(ui.last_focusable.len() == 32, "previous focus order lost")
        ui.note("seen", r)
        for id in 1..33 {
            ui.focusable.push(id)
        }
        ui.end()
    }
    assert(std::alloc::allocations() == before, "warm frame buffers allocated")
    ui.focus = 20
    ui.begin(screen, [Event::KeyDown { key: Key::Tab, repeat: false, mods: Mods {} }])
    assert(ui.focus == 21, "Tab order changed")
    // An empty frame still replaces the previous one; nothing can be found two frames back.
    ui.end()
    ui.begin(screen, [Event::KeyDown { key: Key::Tab, repeat: false, mods: Mods { shift: true } }])
    assert(ui.find("seen").is_none(), "a label survived an empty frame")
    assert(ui.last_focusable.is_empty() && ui.focus == 21, "empty frame focus changed")
    ui.focusable.push(10)
    ui.focusable.push(20)
    ui.focusable.push(30)
    ui.focus = 20
    ui.end()
    ui.begin(screen, [Event::KeyDown { key: Key::Tab, repeat: false, mods: Mods { shift: true } }])
    assert(ui.focus == 10, "Shift+Tab order changed")
}

@test
fn typed_runs_and_keys_match_the_previous_editor() {
    for scenario in 0..10 {
        for cursor in [0, 1, 3, 4, 8, 12] {
            var ui = Ui::new(Fonts::load())
            var old = Ui::new(Fonts::load())
            ui.typed = Vec::from([65, 233, 128512, 66])
            if scenario > 0 {
                let keys = [Key::Backspace, Key::Delete, Key::Left, Key::Right, Key::Home,
                    Key::End, Key::Enter, Key::NumpadEnter, Key::Escape]
                ui.keys.push(keys[scenario - 1])
                ui.typed_before.push(scenario % 5)
                ui.keys.push(Key::Left)
                ui.typed_before.push(4)
            }
            old.typed = ui.typed.clone()
            old.keys = ui.keys.clone()
            old.typed_before = ui.typed_before.clone()
            ui.editing = Some(Editing { id: 7, cursor, before: String::new(), buffer: String::new() })
            old.editing = Some(Editing { id: 7, cursor, before: String::new(), buffer: String::new() })
            var actual = String::from("aßz🌲tail")
            var expected = actual.clone()
            let before = std::alloc::allocations()
            let a = ui.edit_keys(7, mut actual)
            let used = std::alloc::allocations() - before
            let was = std::alloc::allocations()
            let b = old.reference_edit_keys(7, mut expected)
            let old_used = std::alloc::allocations() - was
            assert(actual == expected && a == b && ui.cursor() == old.cursor() &&
                ui.focus == old.focus, "typing behavior changed")
            if scenario == 0 && cursor == 0 {
                assert(old_used >= used + 3, "typing did not save the repeated tail allocations")
            }
        }
    }
}
"#;
    let widgets = format!("{}\n{tests}", source("ui/widgets.wrela"));
    std::fs::write(dir.join("widgets.wrela"), widgets).expect("UI tests");
    // The two focused cases plus the production text, font-height, and widget regressions.
    passes(&dir, 13);
}

#[test]
fn perf_drag_literal_reselection_reuses_room_and_resets_every_value() {
    let reset = function(&source("studio/drag.wrela"), "fn take_literals(");
    let tests = r#"
struct Drag {
    lits: Vec<u32>, original: Vec<f32>, now: Vec<f32>, active: Vec<bool>, held: Vec<bool>,
}

fn value(i: u32) -> f32 { f32(i) * 0.125 }

@test
fn reselection() {
    var d = Drag {
        lits: Vec::new(), original: Vec::new(), now: Vec::new(),
        active: Vec::new(), held: Vec::new(),
    }
    take_literals(mut d, [1, 2, 3, 4, 5, 6, 7, 8])
    for k in 0..8 {
        d.now[k] = -1.0
        d.active[k] = false
        d.held[k] = true
    }
    let before = std::alloc::allocations()
    take_literals(mut d, [8, 2, 8])
    assert(std::alloc::allocations() == before, "reselection allocated")
    assert(d.lits.len() == 3 && d.original.len() == 3 && d.now.len() == 3 &&
        d.active.len() == 3 && d.held.len() == 3, "old literals left over")
    for k in 0..3 {
        let i = [8, 2, 8][k]
        assert(d.lits[k] == i && d.original[k] == value(i) && d.now[k] == value(i) &&
            d.active[k] && !d.held[k], "literal state changed")
    }
    take_literals(mut d, [u32(0); 0])
    take_literals(mut d, [4, 3, 2, 1])
    assert(std::alloc::allocations() == before, "empty reselection lost its room")
}

pub fn frame(time: f32, width: u32, height: u32) {}
"#;
    let dir = package("drag", &format!("{reset}\n{tests}"));
    passes(&dir, 1);
}
