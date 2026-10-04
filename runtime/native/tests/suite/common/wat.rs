//! Test programs as WAT: a program that submits given batches, frame by frame. Shared by the
//! crate's unit tests (`src/program/tests.rs`) and its integration tests.

use std::fmt::Write;

/// A program whose `n`th call of `frame` submits `frames[n]`'s batches, in order (and nothing
/// once `frames` runs out). Its memory is just big enough for the batches.
pub fn program(frames: &[Vec<Vec<u8>>]) -> String {
    let mut data = String::new();
    let mut body = String::new();
    let mut offset = 0usize;
    for (n, batches) in frames.iter().enumerate() {
        let _ = write!(body, "\n    (if (i32.eq (global.get $n) (i32.const {n})) (then");
        for batch in batches {
            let _ = write!(data, "\n  (data (i32.const {offset}) \"{}\")", escape(batch));
            let _ = write!(
                body,
                "\n      (call $submit (i32.const {offset}) (i32.const {}))",
                batch.len()
            );
            offset += batch.len().next_multiple_of(4);
        }
        body.push_str("))");
    }
    let pages = offset.div_ceil(65536).max(1);
    format!(
        r#"(module
  (import "wrela" "submit" (func $submit (param i32 i32)))
  (import "wrela" "memory" (memory {pages} 16384 shared)) (export "memory" (memory 0))
  (global $n (mut i32) (i32.const 0)){data}
  (func (export "frame") (param f32 i32 i32){body}
    (global.set $n (i32.add (global.get $n) (i32.const 1)))))
"#
    )
}

pub fn escape(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("\\{b:02x}")).collect()
}
