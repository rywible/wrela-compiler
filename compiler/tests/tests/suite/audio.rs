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
    assert_eq!(samples.len(), QUANTA as usize * 128);
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
