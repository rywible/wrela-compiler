//! What the command's tests share: a package in a scratch directory, the `wrela` command, and
//! what it prints. Each module uses some of it.

use std::path::PathBuf;
use std::process::Command;

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
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
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
