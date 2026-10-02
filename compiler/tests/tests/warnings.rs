//! Warnings: W0001 (a local bound and never used) and W0002 (an arm that can't match). The
//! conformance suite checks errors only.

use std::path::PathBuf;

fn warnings(name: &str, text: &str) -> Vec<(String, usize)> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("warnings").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let text = format!("{text}\npub fn frame(time: f32, width: u32, height: u32) {{}}\n");
    std::fs::write(dir.join("main.wrela"), &text).expect("write");
    let out = wrela_driver::check(&dir);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    out.diagnostics
        .iter()
        .filter(|d| !d.is_error())
        .map(|d| {
            let span = d.span().expect("warnings have spans");
            let f = out.sources.file(span.file);
            (d.code.to_string(), f.line_index(span.start) + 1)
        })
        .collect()
}

#[test]
fn unused_locals_are_reported_once_each() {
    let src = "fn f(unused_param: i32) -> i32 {
    let a = 1
    let b = 2
    var c = 0
    c = 5
    let _quiet = 3
    let (d, e) = (1, 2)
    for k in 0..3 {
    }
    for _ in 0..3 {
    }
    let g = 4
    let h = |x: i32| x + g
    b + e + h(1)
}";
    let w = warnings("unused", src);
    let lines: Vec<usize> = w.iter().filter(|(c, _)| c == "W0001").map(|(_, l)| *l).collect();
    // `a`, `c` (assigned, never read), `d`, `k`.
    assert_eq!(lines, [2, 4, 7, 8], "{w:?}");
}

#[test]
fn unreachable_arms_are_reported() {
    let src = "fn f(x: u32) -> u32 {
    match x {
        _ => 1,
        3 => 2,
    }
}";
    let w = warnings("unreachable", src);
    assert_eq!(w, [("W0002".to_string(), 4)]);
}
