//! `wrela fix` (AC13): on a package written with Rust's habits, it makes every fix a tool can
//! make and reports each; the package then builds, its files are formatted, and a second run
//! changes nothing. Only the package's own files change, and every place the report names is
//! in the files as they are after.

use crate::common;

use std::path::{Path, PathBuf};

const RUSTY: &str = "struct Log {
    lines: Vec<Text>,
}

struct Ctx {
    log: &Log,
}

struct View<'a> {
    log: &'a mut Log,
}

struct Slot {
    item: Option<u32>,
}

struct P {
    x: f32,
}

impl Clone for P {
    fn clone(self) -> P {
        P { x: self.x }
    }
}

fn grow(x: &mut f32) {
    x = x * 2.0
}

fn first<'a>(xs: &'a Vec<f32>) -> f32 {
    xs[0]
}

fn label(score: u32, name: Text) -> String {
    format!(\"{} scored {:.1}\", name, score)
}

fn empty(s: mut Slot) -> Option<u32> {
    s.item.take()
}

fn total(xs: Vec<f32>) -> f32 {
    var t = 0.0
    for x in xs.iter() {
        t += x
    }
    t
}

fn double(xs: mut Vec<f32>) {
    for x in xs.iter_mut() {
        x *= 2.0
    }
}

pub fn frame(time: f32, width: u32, height: u32) {
    var t = time
    grow(&mut t)
    println!(\"frame {}\", t)
}
";

/// A struct field moved out of a borrowed value: a choice (`borrow`, `.clone()` or `take`).
const CHOICE: &str = "struct Log: Clone {
    lines: Vec<Text>,
}

struct World {
    log: Log,
}

fn lines(w: World) -> u32 {
    let log = w.log
    log.lines.len()
}

pub fn frame(time: f32, width: u32, height: u32) {}
";

/// Runs `wrela`: its exit status, its stdout, and its stdout and stderr together.
fn wrela(args: &[&str]) -> (Option<i32>, String, String) {
    let out = common::wrela().args(args).output().expect("run wrela");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let text = stdout.clone() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code(), stdout, text)
}

/// A new package `name` whose `main.wrela` is `text`.
fn package(name: &str, text: &str) -> PathBuf {
    common::package(name, &[("main.wrela", text)])
}

fn s(p: &Path) -> &str {
    p.to_str().expect("a UTF-8 path")
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).expect("read")
}

#[test]
fn fix_makes_a_rusty_package_build() {
    let dir = package("fix-rusty", RUSTY);
    let (code, _, before) = wrela(&["check", s(&dir)]);
    assert_eq!(code, Some(1), "it doesn't build before:\n{before}");
    let (code, _, report) = wrela(&["fix", s(&dir)]);
    assert_eq!(code, Some(0), "after `wrela fix` it still has errors:\n{report}");
    for code in ["E0106", "E0114", "E0115", "E0207", "E0415"] {
        assert!(report.contains(&format!("fixed {code}")), "no {code} fix in:\n{report}");
    }
    let fixed = read(&dir.join("main.wrela"));
    for want in [
        "borrow struct View",
        "log: mut Log",
        "replace(mut s.item, None)",
        "for mut x in xs",
        "print(f\"frame {t}\")",
        "f\"{name} scored {score:.1}\"",
    ] {
        assert!(fixed.contains(want), "`{want}` isn't in the fixed program:\n{fixed}");
    }
    // Each fix is reported where it is in the fixed, formatted file.
    let lines: Vec<&str> = fixed.lines().collect();
    for (fix, want) in [("E0115: write", "f\"{name}"), ("E0115: print", "print(")] {
        let at = report.lines().find(|l| l.contains(fix)).expect(fix);
        let line: usize = at.split(':').nth(1).and_then(|n| n.parse().ok()).expect(at);
        assert!(lines[line - 1].contains(want), "`{at}` isn't at `{want}` in:\n{fixed}");
    }
    let (code, _, checked) = wrela(&["check", s(&dir)]);
    assert_eq!(code, Some(0), "it doesn't check:\n{checked}");
    let (code, _, fmt) = wrela(&["fmt", "--check", s(&dir)]);
    assert_eq!(code, Some(0), "it isn't formatted:\n{fmt}");
    let (code, _, again) = wrela(&["fix", s(&dir)]);
    assert!(code == Some(0) && again.contains("nothing to fix"), "a second run:\n{again}");
    assert_eq!(read(&dir.join("main.wrela")), fixed);
}

/// A diagnostic with several fixes is a choice for a person: listed, not made.
#[test]
fn fix_leaves_choices_alone() {
    let dir = package("fix-choice", CHOICE);
    let (code, _, report) = wrela(&["fix", s(&dir)]);
    assert_eq!(code, Some(1), "{report}");
    assert!(report.contains("has fixes to choose from"), "{report}");
    assert_eq!(read(&dir.join("main.wrela")), CHOICE);
}

/// With `--json`, the report names each fix made and each file changed, and every place in it
/// is in the files as they are after: a choice's edits can be made from it as they are.
#[test]
fn fix_json_reports_places_in_the_fixed_files() {
    let text = CHOICE.replace("\n\n", "\n\n\n\n")
        + "\nfn show(time: f32) {\n    println!(\"t {}\", time)\n}\n";
    let dir = package("fix-json", &text);
    let (code, json, report) = wrela(&["fix", "--json", s(&dir)]);
    assert_eq!(code, Some(1), "{report}");
    let v: serde_json::Value = serde_json::from_str(&json).expect(&json);
    let fixed = read(&dir.join("main.wrela"));
    assert_ne!(fixed, text, "nothing changed");
    assert_eq!(v["changed"], serde_json::json!(["main.wrela"]), "{json}");
    let made = v["fixed"].as_array().expect("`fixed`");
    assert_eq!(made.len(), 1, "{json}");
    assert_eq!(made[0]["code"], "E0115");
    let line = made[0]["line"].as_u64().expect("a line") as usize;
    assert!(fixed.lines().nth(line - 1).is_some_and(|l| l.contains("print(f\"t {time}\")")));
    let choice = v["diagnostics"]
        .as_array()
        .and_then(|ds| ds.iter().find(|d| d["fixes"].as_array().is_some_and(|f| f.len() > 1)))
        .expect("a choice");
    let span = |v: &serde_json::Value| -> std::ops::Range<usize> {
        v["start"].as_u64().expect("start") as usize..v["end"].as_u64().expect("end") as usize
    };
    assert_eq!(&fixed[span(&choice["span"])], "w.log", "{json}");
    let borrow = &choice["fixes"][0]["edits"][0];
    assert_eq!(&fixed[span(borrow)], "let", "{json}");
}

/// A fix in a dependency's file isn't made, and the report says why.
#[test]
fn fix_changes_only_the_package() {
    let dir = package(
        "fix-dependency",
        "use kit::sum::total\n\npub fn frame(time: f32, width: u32, height: u32) {\n    let _t = total()\n}\n",
    );
    std::fs::write(
        dir.join("wrela.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\nkit = { path = \"kit\" }\n",
    )
    .expect("write");
    std::fs::create_dir_all(dir.join("kit")).expect("mkdir");
    std::fs::write(dir.join("kit/wrela.toml"), "[package]\nname = \"kit\"\n").expect("write");
    let kit = "pub fn total() -> f32 {\n    var t = 0.0\n    for v in [1.0, 2.0].iter() {\n        t += v\n    }\n    t\n}\n";
    std::fs::write(dir.join("kit/sum.wrela"), kit).expect("write");
    let main = read(&dir.join("main.wrela"));
    let (code, _, report) = wrela(&["fix", s(&dir)]);
    assert_eq!(code, Some(1), "{report}");
    assert!(report.contains("E0207's fix wasn't made: it edits `[kit] sum.wrela`"), "{report}");
    assert!(report.contains("no fix made; 1 error left"), "{report}");
    assert_eq!(read(&dir.join("kit/sum.wrela")), kit);
    assert_eq!(read(&dir.join("main.wrela")), main);
}

/// A file that can't be written stops the fixes, but the files written are formatted and
/// reported, and the exit status says so.
#[test]
fn fix_reports_a_file_it_cant_write() {
    let dir = package(
        "fix-read-only",
        "use util::show\n\n\n\npub fn frame(time: f32, width: u32, height: u32) {\n    println!(\"a {}\", time)\n    show(time)\n}\n",
    );
    let util = dir.join("util.wrela");
    std::fs::write(&util, "pub fn show(time: f32) {\n    println!(\"b {}\", time)\n}\n")
        .expect("write");
    let writable = std::fs::metadata(&util).expect("metadata").permissions();
    let mut read_only = writable.clone();
    read_only.set_readonly(true);
    std::fs::set_permissions(&util, read_only).expect("chmod");
    if std::fs::OpenOptions::new().write(true).open(&util).is_ok() {
        // As root, a read-only file can be written: there's nothing to test.
        return;
    }
    let (code, _, report) = wrela(&["fix", s(&dir)]);
    std::fs::set_permissions(&util, writable).expect("chmod");
    assert_eq!(code, Some(2), "{report}");
    assert!(report.contains("can't write") && report.contains("1 fix in 1 file"), "{report}");
    let main = read(&dir.join("main.wrela"));
    assert!(main.contains("print(f\"a {time}\")") && !main.contains("\n\n\n"), "{main}");
}
