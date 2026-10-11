//! `@audio` (AC6, language.md §6.13): compiler/tests/audio's modal-synthesis voice, rendered
//! offline by the native host and in Chrome's AudioWorklet, gives the same samples, bit for bit.

use wrela_host::CpuBuild;
use wrela_tests::page;

/// Two seconds at 48 kHz, in quanta of 128 samples.
const QUANTA: u32 = 750;

/// The voice's samples from the native host's offline run.
fn native(dir: &std::path::Path) -> Vec<f32> {
    let mut host = CpuBuild::load(dir).expect("load").start_with(1).expect("start");
    host.frame(0.0, 16, 16).expect("frame");
    host.render_audio(QUANTA).expect("render")
}

#[test]
fn the_voice_sounds() {
    let (dir, _) = page("compiler/tests/audio", "audio-native");
    let samples = native(&dir);
    assert_eq!(samples.len(), QUANTA as usize * 128 * 2);
    // Two channels, interleaved: the bars are to the left.
    let (l, r): (Vec<f32>, Vec<f32>) = samples.chunks_exact(2).map(|p| (p[0], p[1])).unzip();
    assert!(l.iter().zip(&r).all(|(a, b)| *b == 0.5 * *a));
    // Silent until the first strike, then ringing, within [-1, 1].
    let peak = samples.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    assert!(peak > 0.05 && peak <= 1.0, "peak {peak}");
    assert!(samples.iter().all(|x| x.is_finite()));
    // Rendering again from the start gives the same samples.
    assert_eq!(native(&dir), samples);
}

/// Chrome's AudioWorklet, rendering offline, gives the native host's samples, bit for bit.
#[test]
#[ignore = "long: needs Chrome, python3 and a GPU"]
fn both_hosts_render_the_same_samples() {
    let (dir, rel) = page("compiler/tests/audio", "audio-hosts");
    let want = native(&dir);
    let run =
        wrela_tests::ChromeRun { audio: QUANTA, ..wrela_tests::ChromeRun::new(2, 16, 16, 60.0) };
    let chrome = wrela_tests::run_in_chrome_with(&rel, run).audio;
    assert_eq!(chrome.len(), want.len());
    let first = chrome.iter().zip(&want).position(|(a, b)| a.to_bits() != b.to_bits());
    assert_eq!(first, None, "the samples differ from sample {first:?}");
}

/// engine::sound's mixer at its busiest (compiler/tests/mixer: the listener, the wind, four
/// waters, three brushes, four steps, two impacts and two whooshes, all sounding): a second from
/// the native host.
fn mixed(dir: &std::path::Path) -> Vec<f32> {
    let mut host = CpuBuild::load(dir).expect("load").start_with(1).expect("start");
    host.frame(0.0, 16, 16).expect("frame");
    host.render_audio(MIXED).expect("render")
}

/// A second at 48 kHz, in quanta of 128 samples.
const MIXED: u32 = 375;

/// The mixer's second: in [-1, 1], louder to the left than the right (the sources are placed
/// round the listener, not ahead), the same when rendered again, and hashing as it did when the
/// mixer was made (`MIXED_HASH`: it changes only when the mixer's sound does).
#[test]
fn the_mixer_mixes_the_same_samples_every_time() {
    let (dir, _) = page("compiler/tests/mixer", "mixer-native");
    let samples = mixed(&dir);
    assert_eq!(samples.len(), MIXED as usize * 128 * 2);
    let peak = samples.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    assert!(peak > 0.01 && peak < 1.0, "peak {peak}");
    let (l, r): (Vec<f32>, Vec<f32>) = samples.chunks_exact(2).map(|p| (p[0], p[1])).unzip();
    let energy = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>();
    assert!((energy(&l) / energy(&r)).log10().abs() > 0.05, "no side louder");
    assert_eq!(mixed(&dir), samples);
    let mut hash = wrela_abi::hash::StateHash::new();
    for x in &samples {
        hash.update(&x.to_le_bytes());
    }
    assert_eq!(hash.hex(), MIXED_HASH);
}

const MIXED_HASH: &str = "6f33c8535e5a87db";

/// Chrome's AudioWorklet, rendering the mixer offline, gives the native host's samples, bit for
/// bit; and at its busiest it takes under a quarter of a core.
#[test]
#[ignore = "long: needs Chrome, python3 and a GPU"]
fn both_hosts_mix_the_same_samples() {
    let (dir, rel) = page("compiler/tests/mixer", "mixer-hosts");
    let want = mixed(&dir);
    let run =
        wrela_tests::ChromeRun { audio: MIXED, ..wrela_tests::ChromeRun::new(2, 16, 16, 60.0) };
    let chrome = wrela_tests::run_in_chrome_with(&rel, run).audio;
    assert_eq!(chrome.len(), want.len());
    let first = chrome.iter().zip(&want).position(|(a, b)| a.to_bits() != b.to_bits());
    assert_eq!(first, None, "the samples differ from sample {first:?}");
    // The voice's budget is a quarter of a core (vision.md; #56): 4 times real time or faster.
    let ms =
        wrela_tests::result_json(&dir.join("results"), "audio-ms.json")["ms"].as_f64().expect("ms");
    let times = f64::from(MIXED) * 128.0 / 48.0 / ms;
    println!("Chrome mixed a second in {ms:.0} ms: {times:.1} times real time");
    assert!(times >= 4.0, "the mixer at its busiest takes over a quarter of a core: {times:.1}x");
}
