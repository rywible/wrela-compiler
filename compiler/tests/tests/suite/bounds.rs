//! A debug build's bounds checks on the GPU (AC4, language.md §11). WGSL doesn't trap on an
//! index out of range, so a debug build checks each index in GPU code, and a check that fails
//! sets a flag the host reads after each frame. compiler/tests/bounds reads one element past
//! its buffer from 0.05 s on. Need a GPU (and Chrome):
//! `cargo test -p wrela-tests --test suite bounds:: -- --ignored`.

use wrela_host::Host;
use wrela_tests::{ChromeRun, chrome_failure, debug_page, page};

const MESSAGE: &str = "debug build: pipeline `spill` indexed an array out of range";

/// What `spill` writes in range (`past` 0): each input, its table entry, and the guarded entry
/// before it, which only invocations 1 to 4 read.
fn expected() -> Vec<u32> {
    let table = [10, 20, 30, 40];
    (0..64u32)
        .map(|i| {
            let before = if (1..=4).contains(&i) { table[i as usize - 1] } else { 0 };
            i + table[i as usize % 4] + before
        })
        .collect()
}

/// The kernel's output: the newest of the program's buffers.
fn output(host: &mut Host) -> Vec<u32> {
    let out = *host.buffers().last().expect("the output buffer");
    wrela_tests::u32s(&host.read_buffer(out).expect("read the output"))
}

#[test]
#[ignore = "needs a GPU"]
fn the_native_host_reports_an_index_out_of_range() {
    let (dir, _) = debug_page("compiler/tests/bounds", "bounds-native-debug");
    let mut host = Host::load(&dir).expect("load");
    // In range at 0 s: the frame ends cleanly.
    host.run_frames(&[0.0], 4, 4).expect("an index in range");
    assert_eq!(output(&mut host), expected(), "the guarded read's values");
    let err = host.run_frames(&[0.1], 4, 4).expect_err("an index out of range").to_string();
    assert!(err.contains(MESSAGE), "{err}");
}

/// A release build doesn't check: the read past the end gives what WGSL gives.
#[test]
#[ignore = "needs a GPU"]
fn a_release_build_does_not_check() {
    let (dir, _) = page("compiler/tests/bounds", "bounds-native-release");
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).expect("manifest");
    assert!(!manifest.contains("debug_flag"), "a release build with a debug flag");
    let mut host = Host::load(&dir).expect("load");
    host.run_frames(&[0.0], 4, 4).expect("an index in range");
    assert_eq!(output(&mut host), expected(), "the guarded read's values");
    host.run_frames(&[0.1], 4, 4).expect("no checks");
}

#[test]
#[ignore = "needs the runtime"]
fn chrome_reports_an_index_out_of_range() {
    let (_, rel) = debug_page("compiler/tests/bounds", "bounds-chrome-debug");
    // Frame 3 is at 0.05 s.
    let failure = chrome_failure(&rel, ChromeRun::new(6, 4, 4, 60.0));
    assert!(failure.contains(MESSAGE), "{failure}");
}
