//! `wrela audio` on a piece (#56 AC1): examples/piano's one note, in a scratch copy, through each
//! tool that renders or describes a take: `wav`, `describe`, `numbers`, `midi`, `sheet` and
//! `speed`, and `partials` and `clicks` on the WAV it wrote. (The measuring tools' own numbers
//! are tested in the command's unit tests, on notes made there.)

use crate::common;

use std::path::{Path, PathBuf};

/// examples/piano in a scratch directory `name`, with the engine by its path in the repository.
fn bench(name: &str) -> PathBuf {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let engine = repo.join("engine").canonicalize().expect("the engine");
    let main = std::fs::read_to_string(repo.join("examples/piano/main.wrela")).expect("main");
    let manifest = format!(
        "[package]\nname = \"piano\"\n\n[dependencies]\nengine = {{ path = {:?} }}\n",
        engine.display().to_string()
    );
    common::package(name, &[("wrela.toml", &manifest), ("main.wrela", &main)])
}

#[test]
fn audio_renders_describes_and_measures_a_take() {
    let dir = bench("audio-bench");
    let d = dir.to_str().unwrap();
    // A4, struck hard and held: a second of it.
    let take = ["--take", "69,100,0", "--seconds", "1"];
    let run = |args: &[&str]| common::answer(common::wrela().arg("audio").args(args).args(take));

    let wav = dir.join("a4.wav");
    let w = run(&[d, "wav", wav.to_str().unwrap()]);
    assert_eq!(w["seconds"], 1.0);
    assert_eq!(w["clipped"], 0);
    let peak = w["peak_db"].as_f64().unwrap();
    assert!(peak > -40.0 && peak < 0.0, "peak {peak} dB");
    // Stereo floats at 48 kHz: a header, then a second of two channels.
    let bytes = std::fs::read(&wav).expect("the WAV");
    assert!(bytes.len() >= 2 * 48000 * 4, "{} bytes", bytes.len());

    let t = run(&[d, "describe"]);
    assert_eq!(t["notes"].as_array().unwrap().len(), 1);
    assert_eq!(t["notes"][0]["key"], 69);
    assert_eq!(t["voices"], serde_json::json!(["note"]));

    let n = run(&[d, "numbers"]);
    assert_eq!(n["notes"], 1);
    assert!(n["rms_db"].as_f64().unwrap() < n["peak_db"].as_f64().unwrap());

    let mid = dir.join("a4.mid");
    assert_eq!(run(&[d, "midi", mid.to_str().unwrap()])["notes"], 1);
    let midi = std::fs::read(&mid).expect("the MIDI file");
    assert_eq!(&midi[..4], b"MThd");
    // The note on: key 69 at velocity 100.
    assert!(midi.windows(3).any(|w| w == [0x90, 69, 100]));

    let png = dir.join("a4.png");
    run(&[d, "sheet", png.to_str().unwrap()]);
    assert_eq!(&std::fs::read(&png).expect("the sheet")[1..4], b"PNG");

    let s = run(&[d, "speed"]);
    assert_eq!(s["quanta"], 375);
    assert!(s["times_real_time"].as_f64().unwrap() > 1.0, "{s}");

    // The note's partials, from the WAV: A4's first is at 440 Hz, near enough.
    let wav = wav.to_str().unwrap();
    let p = common::answer(common::wrela().args([
        "audio",
        "partials",
        wav,
        "--key",
        "69",
        "--seconds",
        "0.5",
    ]));
    let f0 = p["f0_hz"].as_f64().unwrap();
    assert!((f0 - 440.0).abs() < 4.0, "A4 at {f0} Hz");
    let c = common::answer(common::wrela().args(["audio", "clicks", wav]));
    assert_eq!(c["seconds"], 1.0);
    assert!(c["clicks"].is_u64(), "{c}");
}
