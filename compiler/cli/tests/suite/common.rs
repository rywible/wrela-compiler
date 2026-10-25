//! What the command's tests share: a package in a scratch directory, the `wrela` command, what
//! it prints, waits on processes, and the files a run leaves. Each module uses some of it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// A program with a struct, a trait, an impl, generics, borrows and moves: what queries and
/// refactors read.
pub const SHAPES: &str = r#"use std::collections::Vec

/// A row of heights.
pub struct Grid {
    pub cells: Vec<f32>,
}

impl Grid {
    /// The height at `i`.
    pub fn at(self, i: u32) -> f32 {
        self.cells[i]
    }
}

pub trait Shape {
    fn area(self) -> f32
}

pub struct Square: Copy {
    pub side: f32,
}

impl Shape for Square {
    fn area(self) -> f32 {
        self.side * self.side
    }
}

fn total<S: Shape + Copy>(shapes: [S]) -> f32 {
    var sum = 0.0
    for s in shapes {
        sum += s.area()
    }
    sum
}

fn grow(g: mut Grid) {
    g.cells.push(1.0)
}

fn first(g: Grid) -> f32 {
    g.at(0)
}

pub fn frame(time: f32, width: u32, height: u32) {
    var g = Grid { cells: Vec::new() }
    grow(mut g)
    borrow c = g.cells
    let n = c.len()
    let a = first(g) + f32(n)
    let squares = [Square { side: a }, Square { side: 2.0 }]
    let h = take g
    let t = total(squares) + h.at(0)
}
"#;

/// A `main.wrela` with nothing in it but `frame`, for a package whose code is in its modules.
pub const FRAME: &str = "pub fn frame(time: f32, width: u32, height: u32) {}\n";

/// A `wrela.toml` for the package `name`.
pub fn manifest(name: &str) -> String {
    format!("[package]\nname = \"{name}\"\n")
}

/// A package in a scratch directory of its own, `name`, of `files` (each a path in it and its
/// text): the tests run at once, and one that shared a directory with another could remove it
/// while the other built there.
pub fn package(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::var_os("WRELA_TEST_TMPDIR")
        .filter(|d| !d.is_empty())
        .map_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")), PathBuf::from);
    assert!(root.is_absolute(), "WRELA_TEST_TMPDIR must be an absolute path");
    let dir = root.join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    for (path, text) in files {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().expect("a directory")).expect("mkdir");
        std::fs::write(file, text).expect("write");
    }
    dir
}

/// The `wrela` command, to give arguments to.
pub fn wrela() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wrela"))
}

/// Runs `command`: its standard output, as JSON (or a panic that shows what it printed).
pub fn json(command: &mut Command) -> serde_json::Value {
    let out = command.output().expect("run wrela");
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// Runs `command`, which must succeed: its standard output.
pub fn stdout(command: &mut Command) -> String {
    let out = command.output().expect("run wrela");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{text}{}", String::from_utf8_lossy(&out.stderr));
    text
}

/// Runs `command`, which must succeed: its standard output, as JSON.
pub fn answer(command: &mut Command) -> serde_json::Value {
    serde_json::from_str(&stdout(command)).expect("JSON")
}

/// Runs `wrela` with `args`: its exit code, its standard output, and its standard output and
/// error together.
pub fn outputs(args: &[&str]) -> (Option<i32>, String, String) {
    let out = wrela().args(args).output().expect("run wrela");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let all = stdout.clone() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code(), stdout, all)
}

/// Runs `command`: its exit code, and its standard output and error together.
pub fn run(command: &mut Command) -> (Option<i32>, String) {
    let out = command.output().expect("run wrela");
    let text =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.code(), text)
}

/// Waits up to `bound` for `done`.
pub fn within(bound: Duration, mut done: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < bound {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    done()
}

/// `child`'s exit code if it ends within `bound`; else it is killed, and `None`.
pub fn exit_within(child: &mut Child, bound: Duration) -> Option<i32> {
    let started = Instant::now();
    while started.elapsed() < bound {
        if let Some(status) = child.try_wait().expect("wait") {
            return status.code();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// Whether a process of the process group `group` still runs.
pub fn group_alive(group: i32) -> bool {
    Command::new("kill")
        .args(["-0", &format!("-{group}")])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Every file under `dir`.
pub fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// Every file under `dir`, by its path there, with its bytes.
pub fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read the directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let name = path.strip_prefix(dir).expect("inside").to_string_lossy().into_owned();
                out.insert(name, std::fs::read(&path).expect("read"));
            }
        }
    }
    out
}
