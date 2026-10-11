//! Spike 14 (#48): a piano piece as a program. engine::score reads notation in constants, and a
//! mistake in it fails the build where it is; examples/gymnopedie's score and performance pass
//! their own tests; every number of its taste and marks is lifted; engine::piano renders the
//! same samples every time, and in Chrome the same as on the native host, in real time without
//! dropping out (long checks: a minute of the piece, the whole of it at full size).

use super::{engine_package, tests_pass};
use std::path::Path;
use wrela_host::CpuBuild;
use wrela_tests::{page, repo_root};

/// The build error a score's text makes: E0704's message.
fn score_error(name: &str, voices: &str) -> String {
    let text = format!(
        "use engine::score::{{Score, meter, score}}\n\nconst S: Score = score(\"s\", meter(beats: 3, unit: 4)){voices}\n"
    );
    let dir = engine_package(&format!("piano/{name}"), "scores", &text);
    let out = wrela_driver::check(&dir);
    let e = out.diagnostics.iter().find(|d| d.code.as_str() == "E0704").unwrap_or_else(|| {
        panic!(
            "{name}: no E0704 in {:?}",
            out.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        )
    });
    e.message.clone()
}

#[test]
fn a_mistake_in_a_score_fails_the_build_where_it_is() {
    let cases = [
        (
            "short_bar",
            r#".voice("melody", ["e4/4 g4 b4 | c5/2 | d5/2. |"])"#,
            "s, voice `melody`, bar 2, beat 3: bar 2 holds 2 beats, but 3/4 has 3",
        ),
        (
            "long_bar",
            r#".voice("melody", ["e4/4 g4 b4 c5 |"])"#,
            "bar 1 holds 4 beats, but 3/4 has 3",
        ),
        (
            "off_the_keyboard",
            r#".voice("melody", ["g#0/2. |"])"#,
            "this pitch isn't one of the piano's 88 keys (A0 to C8)",
        ),
        ("no_octave", r#".voice("melody", ["c9/2. |"])"#, "a pitch needs an octave from 0 to 8"),
        (
            "lost_tie",
            r#".voice("melody", ["e4/2.~ | f4/2. |"])"#,
            "bar 2, beat 1: a tie continues to no note of the same pitch",
        ),
        ("open_hairpin", r#".voice("melody", ["e4/2. !< | f4/2. |"])"#, "a hairpin isn't ended"),
        ("open_slur", r#".voice("melody", ["e4/2. ( | f4/2. |"])"#, "a slur isn't ended"),
        ("unknown_mark", r#".voice("melody", ["e4/2. !loud |"])"#, "`!loud` isn't a mark"),
        (
            "voices_disagree",
            r#".voice("melody", ["e4/2. | f4/2. |"]).voice("bass", ["e2/2. |"])"#,
            "s: voice `bass` has 1 bars, but the voices before it have 2",
        ),
    ];
    for (name, voices, want) in cases {
        let got = score_error(name, voices);
        assert!(got.contains(want), "{name}: {got}");
    }
}

/// The piece's own tests: its score has 78 bars in four voices, and its deadpan plays the score
/// exactly.
#[test]
fn the_gymnopedies_tests_pass() {
    assert_eq!(tests_pass(&repo_root().join("examples/gymnopedie")), 2);
}

/// Every number of the taste and the marks is lifted, so the lens's tools can change it while
/// the piece plays (AC4).
#[test]
fn every_number_of_the_performance_is_lifted() {
    let dir = super::lift::build_into(
        "piano-lift",
        &repo_root().join("examples/gymnopedie"),
        &["gymnopedie"],
        false,
    );
    let r = super::lift::report(&dir);
    let files = r["files"].as_array().expect("files");
    let file = files
        .iter()
        .position(|f| f["path"] == "performance.wrela")
        .expect("performance.wrela is lifted");
    let in_performance = |list: &str| -> Vec<serde_json::Value> {
        r[list].as_array().expect("a list").iter().filter(|l| l["file"] == file).cloned().collect()
    };
    // The report lists every float literal, lifted or not: none left out is every one lifted.
    let lifted = in_performance("literals");
    assert!(lifted.len() >= 30, "{} literals lifted", lifted.len());
    assert_eq!(in_performance("not_lifted"), Vec::<serde_json::Value>::new());
}

/// The piece's samples from the native host's offline run: `seconds` of take 1.
fn native(dir: &Path, seconds: f64) -> Vec<f32> {
    let mut host = CpuBuild::load(dir).expect("load").start_with(1).expect("start");
    host.frame(0.0, 16, 16).expect("frame");
    host.render_audio((seconds * 48000.0 / 128.0).ceil() as u32).expect("render")
}

/// The piece's first notes, rendered twice: the same samples. (The left hand's first chord is on
/// beat 2 of bar 1; two seconds hold it.)
#[test]
fn the_piano_renders_the_same_samples_every_time() {
    let (dir, _) = page("examples/gymnopedie", "gymnopedie-native");
    let samples = native(&dir, 2.0);
    let peak = samples.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    assert!(peak > 0.01 && peak < 1.0, "peak {peak}");
    assert!(samples.iter().all(|x| x.is_finite()));
    assert_eq!(native(&dir, 2.0), samples);
}

/// The port to main (#56 AC1) renders spike 14's samples: the performance with its room, as
/// 16-bit samples the way the spike wrote round 3's `wrela-performance-with-room.wav` (branch
/// `spike-14`), hashes as that file's samples do.
#[test]
#[ignore = "long: renders the whole piece"]
fn the_piece_renders_spike_14s_samples() {
    let (dir, _) = page("examples/gymnopedie", "gymnopedie-spike");
    let samples = native(&dir, 223.36776733398438);
    let mut hash = wrela_abi::hash::StateHash::new();
    for x in &samples[..SPIKE_SAMPLES] {
        let q = (f64::from(*x) * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
        hash.update(&q.to_le_bytes());
    }
    assert_eq!(hash.hex(), SPIKE_HASH);
}

/// Round 3's WAV: its samples (both channels), and the FNV-1a 64 of their bytes.
const SPIKE_SAMPLES: usize = 21_443_328;
const SPIKE_HASH: &str = "79f613935b8fcd1d";

/// Seconds of the piece the long checks play: a minute, or the whole piece and its room's tail
/// at full size.
fn long_seconds() -> f64 {
    wrela_tests::sized(60.0, 228.0)
}

/// Chrome's AudioWorklet, rendering the piece offline, gives the native host's samples, bit for
/// bit; and how fast it renders, as a multiple of real time.
#[test]
#[ignore = "long: needs Chrome, python3 and a GPU"]
fn both_hosts_render_the_piece_the_same() {
    let (dir, rel) = page("examples/gymnopedie", "gymnopedie-hosts");
    let seconds = long_seconds();
    let want = native(&dir, seconds);
    let quanta = (seconds * 48000.0 / 128.0).ceil() as u32;
    let run =
        wrela_tests::ChromeRun { audio: quanta, ..wrela_tests::ChromeRun::new(2, 16, 16, 60.0) };
    let chrome = wrela_tests::run_in_chrome_with(&rel, run).audio;
    assert_eq!(chrome.len(), want.len());
    let first = chrome.iter().zip(&want).position(|(a, b)| a.to_bits() != b.to_bits());
    assert_eq!(first, None, "the samples differ from sample {first:?}");
    let ms =
        wrela_tests::result_json(&dir.join("results"), "audio-ms.json")["ms"].as_f64().expect("ms");
    println!(
        "Chrome rendered {seconds} s in {:.0} ms: {:.1} times real time",
        ms,
        seconds * 1000.0 / ms
    );
}

/// In real time, Chrome plays the piece with no underrun, where it reports underruns
/// (`AudioContext.playbackStats`).
#[test]
#[ignore = "long: needs Chrome, python3 and a GPU"]
fn chrome_plays_the_piece_in_real_time() {
    let (dir, rel) = page("examples/gymnopedie", "gymnopedie-live");
    let seconds = long_seconds();
    let run = wrela_tests::ChromeRun {
        live: seconds as u32,
        ..wrela_tests::ChromeRun::new(2, 16, 16, 60.0)
    };
    wrela_tests::run_in_chrome_with(&rel, run);
    let live = wrela_tests::result_json(&dir.join("results"), "live.json");
    println!("{live}");
    let played = live["played"].as_f64().expect("played");
    assert!(played > 0.95 * seconds, "the context didn't run: {live}");
    let stats = &live["playbackStats"];
    if let Some(events) = stats.get("underrunEvents").and_then(serde_json::Value::as_f64) {
        assert_eq!(events, 0.0, "{live}");
    }
}
