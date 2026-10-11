//! Requests (language.md §6.15, AC6): compiler/tests/requests makes a readback, a store and a
//! load of its storage, and two fetches (one of a missing file), and polls them on later frames.
//! The native host answers each as the program expects, and headless Chrome gives the same
//! answers: its state hash, which covers a buffer write of every answer, is the native host's.
//! `cargo test -p wrela-tests --test suite requests:: -- --ignored`.

use std::path::Path;
use wrela_host::{Host, Options, frame_time};
use wrela_tests::{page, run_in_chrome};

const FRAMES: u32 = 41;
/// The file the program fetches, in its build.
const LEVEL: [u8; 6] = [10, 20, 30, 40, 50, 60];

fn with_level(dir: &Path) {
    std::fs::create_dir_all(dir.join("data")).expect("data dir");
    std::fs::write(dir.join("data/level.bin"), LEVEL).expect("write the level");
}

fn native(dir: &Path) -> (String, [f32; 4]) {
    let storage = dir.join("storage");
    let _ = std::fs::remove_dir_all(&storage);
    let options = Options { storage: Some(storage), ..Options::default() };
    let mut host = Host::load_with(dir, &options).expect("load");
    let times: Vec<f32> = (0..FRAMES).map(|i| frame_time(i, 60.0)).collect();
    let run = host.run_frames(&times, 16, 16).expect("run");
    let answers = wrela_tests::one_vec4(&mut host, "answers", &[]);
    (run.hash_hex(), answers)
}

#[test]
#[ignore = "needs a GPU"]
fn requests_are_answered_on_later_frames() {
    let (dir, _) = page("compiler/tests/requests", "requests-native");
    with_level(&dir);
    let (_, [readback, saved_missing, loaded, fetched]) = native(&dir);
    // The kernel wrote i² for i < 64; each answer is 1 + its sum (0 would be "not yet").
    let squares: u32 = (0..64).map(|i| i * i).sum();
    assert_eq!(readback, (1 + squares) as f32, "the readback");
    assert_eq!(saved_missing, 11.0, "the store succeeded and the missing file failed");
    assert_eq!(loaded, 16.0, "the load gave back what was stored: 1 + 1 + 2 + 3 + 4 + 5");
    let level: u32 = LEVEL.iter().map(|&b| u32::from(b)).sum();
    assert_eq!(fetched, (1 + level) as f32, "the fetch");
    let stored = std::fs::read(dir.join("storage/saves/slot")).expect("the stored file");
    assert_eq!(stored, [1, 2, 3, 4, 5]);
}

#[test]
#[ignore = "long: needs Chrome, python3 and a GPU"]
fn the_browser_gives_the_same_answers() {
    let (dir, rel) = page("compiler/tests/requests", "requests-browser");
    with_level(&dir);
    let browser = run_in_chrome(&rel, FRAMES, 16, 16, 60.0);
    let (hash, _) = native(&dir);
    assert_eq!(browser.hash, hash, "the hosts' answers differ (their state hashes do)");
}

/// `std::io::print` puts its line on the host's console: the native host prints it, and keeps it
/// for a test to read. `@deterministic` code prints too (`trace`, not `io`); `@audio` code
/// doesn't.
#[test]
fn a_printed_line_reaches_the_host() {
    let text = "use std::io::print

pub fn frame(time: f32, width: u32, height: u32) {
    print(f\"frame at {time:.1} s, {width} wide\")
}
";
    let dir = crate::package("requests-log", text);
    let out = dir.join("build");
    wrela_tests::must_build(&dir, &out);
    let mut host = wrela_host::CpuHost::load(&out).expect("load");
    host.frame(0.5, 64, 48).expect("frame");
    assert_eq!(host.take_logs(), ["frame at 0.5 s, 64 wide"]);
    let det = "use std::io::print

@deterministic
fn step(n: u32) -> u32 {
    print(\"step\")
    n + 1
}

@audio
fn render(level: mut f32, out: mut [f32]) {
    print(\"quantum\")
}
";
    let built = wrela_driver::check(&crate::package("requests-log-det", det));
    let errors: Vec<&wrela_diag::Diagnostic> =
        built.diagnostics.iter().filter(|d| d.is_error()).collect();
    assert_eq!(errors.len(), 1, "only the audio thread's print is an error: {errors:?}");
    assert_eq!(errors[0].code.as_str(), "E0600");
    assert!(errors[0].message.contains("`render`"), "{}", errors[0].message);
}

/// The clock (`std::time`, M6): it never goes back, and work takes time on it. In a constant it's
/// the `nondet` effect's error (and in `@deterministic` code: `eff.clock`; parallel work can't
/// read it either, as it can't be `nondet`).
#[test]
fn the_clock_goes_forward() {
    let text = "use std::time::{now, since}

fn spin(n: u32) -> u32 {
    var x: u32 = 1
    for i in 0..n {
        x = x.wrapping_mul(1664525).wrapping_add(i)
    }
    x
}

/// The seconds in all, the seconds a loop took, and the seconds between two readings in a row.
pub fn laps() -> vec3 {
    let a = now()
    let t = now()
    let x = spin(4000000)
    let took = since(t) + f32(x % 2) * 0.0
    let b = now()
    let c = now()
    vec3(f32(b - a), took, f32(c - b))
}

pub fn frame(time: f32, width: u32, height: u32) {}
";
    let dir = crate::package("requests-clock", text);
    let out = dir.join("build");
    wrela_tests::must_build(&dir, &out);
    let mut host = wrela_host::CpuHost::load(&out).expect("load");
    let [total, took, between] = wrela_tests::one_vec3(&mut host, "laps", &[]);
    assert!(took > 0.0, "work takes time: {took} s");
    assert!(total >= took && between >= 0.0, "the clock went back: {total}, {took}, {between}");
    let in_const = "use std::time::now\n\nconst AT: f64 = now()\n\npub fn frame(time: f32, width: u32, height: u32) {}\n";
    let built = wrela_driver::check(&crate::package("requests-clock-const", in_const));
    let codes: Vec<&str> = built.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(codes, ["E0600"], "a constant can't read the clock");
}

/// A line printed and a phase timed are outputs (`trace`, §8, `eff.trace`): a constant's and a
/// test's come back with the build's and the run's reports (their lines shown as they were
/// made), and a program's line reaches the host though the call traps right after it, which the
/// stream's `Log` (until version 8) didn't.
#[test]
fn prints_and_phases_are_outputs() {
    let text = "use std::io::print
use std::time::phase

fn summed(n: u32) -> u32 {
    var total = 0
    for i in 0..n {
        phase(\"sum\")
        total += i
    }
    phase(\"report\")
    print(f\"summed {total}\")
    total
}

const SUMMED: u32 = summed(10)

@test
fn sums() {
    phase(\"check\")
    print(\"checking\")
    assert(SUMMED == 45)
}

pub fn shout() {
    print(\"before the trap\")
    panic(\"after it\")
}

pub fn frame(time: f32, width: u32, height: u32) {}
";
    let dir = crate::package("requests-trace", text);
    let built = wrela_tests::build(&dir).unwrap_or_else(|e| panic!("{e}"));
    let traced = &built.consts.traced;
    assert_eq!(traced.len(), 1, "one constant traced: {traced:?}");
    let (name, summed) = &traced[0];
    assert_eq!(name, "SUMMED");
    assert_eq!(summed.lines, ["summed 45"]);
    let phases: Vec<(&str, u32)> =
        summed.phases.iter().map(|p| (p.name.as_str(), p.times)).collect();
    assert_eq!(phases, [("sum", 10), ("report", 1)]);
    let tested = wrela_driver::test(&dir, None);
    assert!(tested.passed(), "{:?}", tested.results);
    assert_eq!(tested.results[0].traced.lines, ["checking"]);
    assert_eq!(tested.results[0].traced.phases[0].name, "check");
    let out = dir.join("build");
    built.write_to(&out).expect("write the build");
    let mut host = wrela_host::CpuHost::load(&out).expect("load");
    assert!(host.call_export("shout", &[]).is_err(), "it panics");
    assert_eq!(host.take_logs(), ["before the trap"]);
}

/// Commands larger than the host's command buffer (1 MiB, M6 AC15), in both hosts: a texture
/// and a buffer written whole (4 MB each) go in pieces; a store of 2 MB panics, naming `store`;
/// and any other command over it (a label 2 MB long) panics with the backstop's message. Spike 17
/// found a texture's write trapped with no message (`unreachable`).
const STORE_TOO_LARGE: &str = "store: the bytes and the path are more than one request carries";
const COMMAND_TOO_LARGE: &str = "a GPU or IO command larger than the host's command buffer";

#[test]
#[ignore = "needs a GPU"]
fn a_command_over_the_buffer_is_split_or_named() {
    let (dir, _) = page("compiler/tests/large-commands", "large-commands-native");
    let mut host = Host::load(&dir).expect("load");
    host.run_frames(&[0.0, 1.0 / 60.0], 4, 4).expect("large writes go in pieces");
    let err = host.run_frames(&[2.0 / 60.0], 4, 4).expect_err("the store is too large");
    assert!(err.to_string().contains(STORE_TOO_LARGE), "{err}");
    let mut cpu = wrela_host::CpuHost::load(&dir).expect("load");
    let err = cpu.call_export("big_label", &[]).expect_err("the label is too large");
    assert!(err.to_string().contains(COMMAND_TOO_LARGE), "{err}");
}

#[test]
#[ignore = "long: needs Chrome and a GPU"]
fn chrome_splits_or_names_a_command_over_the_buffer() {
    let (_, rel) = page("compiler/tests/large-commands", "large-commands-chrome");
    let failure = wrela_tests::chrome_failure(&rel, wrela_tests::ChromeRun::new(4, 4, 4, 60.0));
    assert!(failure.contains(STORE_TOO_LARGE), "{failure}");
}

/// The frames the shipped-files program runs: it fetches on the first, writes on the 40th.
const SHIPPED_FRAMES: u32 = 41;

fn shipped_native(dir: &Path) -> (String, [f32; 4]) {
    let mut host = Host::load_with(dir, &Options::default()).expect("load");
    let times: Vec<f32> = (0..SHIPPED_FRAMES).map(|i| frame_time(i, 60.0)).collect();
    let run = host.run_frames(&times, 16, 16).expect("run");
    let answers = wrela_tests::one_vec4(&mut host, "answers", &[]);
    (run.hash_hex(), answers)
}

/// Files a build ships (`std::io::Shipped`, M6): compiler/tests/shipped's constant is two files
/// under the build's `files/data/`, not in its WASM; the program knows their names and sizes,
/// fetches each, and gets its bytes; a constant computed from it read them in place.
#[test]
#[ignore = "needs a GPU"]
fn shipped_files_are_beside_the_program_and_fetched() {
    let (dir, _) = page("compiler/tests/shipped", "shipped-native");
    let big: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(std::fs::read(dir.join("files/data/big.bin")).expect("big.bin"), big);
    assert_eq!(
        std::fs::read(dir.join("files/data/small.bin")).expect("small.bin"),
        [3, 1, 4, 1, 5]
    );
    let wasm = std::fs::read(dir.join("game.wasm")).expect("game.wasm");
    assert!(
        !wasm.windows(256).any(|w| w == &big[1000..1256]),
        "the program's WASM holds the shipped file's bytes"
    );
    let (_, [small, fetched, counts, first]) = shipped_native(&dir);
    assert_eq!(small, 15.0, "small.bin fetched: 1 + 3 + 1 + 4 + 1 + 5");
    let big_sum: u32 = big.iter().map(|&b| u32::from(b)).sum();
    assert_eq!(fetched, (1 + big_sum) as f32, "big.bin fetched");
    assert_eq!(counts, (2 * 100_000 + 5 + 5000) as f32, "two files, their sizes known");
    assert_eq!(first, 14.0, "build-time code read small.bin in place");
}

#[test]
#[ignore = "long: needs Chrome, python3 and a GPU"]
fn chrome_fetches_shipped_files_alike() {
    let (dir, rel) = page("compiler/tests/shipped", "shipped-browser");
    let browser = run_in_chrome(&rel, SHIPPED_FRAMES, 16, 16, 60.0);
    let (hash, _) = shipped_native(&dir);
    assert_eq!(browser.hash, hash, "the hosts' answers differ (their state hashes do)");
}
