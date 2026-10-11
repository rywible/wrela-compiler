//! `wrela audio`: tools for a program that plays a piece on its voice (spike 14, #48). An agent
//! can't listen, so these say what a take sounds like in forms it can read: the samples as a
//! WAV file for a person, and for the agent the notes as played, a summary in numbers, a MIDI
//! file for another instrument, and a sheet (a PNG of the piano roll, the tempo, the pedals and
//! a spectrogram).
//!
//! ```text
//! wrela audio <package-dir> wav <out.wav> [--take n] [--seconds s]
//! wrela audio <package-dir> describe [--take n]
//! wrela audio <package-dir> numbers [--take n]
//! wrela audio <package-dir> midi <out.mid> [--take n]
//! wrela audio <package-dir> sheet <out.png> [--take n] [--bars a-b]
//! wrela audio <package-dir> speed [--take n]
//! wrela audio partials <file.wav> --key k [--from s] [--seconds s]
//! wrela audio model <dir> --out <file.wrela> [--against <dir>]
//! wrela audio clicks <file.wav> [--notes <take.json>]
//! ```
//!
//! A piece is a program with three exports: `choose(...)`, called before its first frame
//! with `--take`'s numbers (`--take 1`, or `--take 60,64` for two), which says which take it
//! plays (the program's own choice without `--take`); `seconds() -> f32`, how long the take sounds; and `describe()`, which
//! prints the take as JSON lines (`examples/gymnopedie` has them). The program is built as a
//! release build and run on the native host's CPU; the voice is rendered offline, so the
//! samples are the ones Chrome's AudioWorklet renders too.

mod clicks;
mod dsp;
mod measure;
mod model;
mod sheet;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wrela_host::{CpuHost, Value};

struct Args {
    dir: PathBuf,
    action: String,
    rest: Vec<String>,
    take: Option<Vec<u32>>,
    seconds: Option<f64>,
    bars: Option<(u32, u32)>,
    key: Option<u32>,
    from: f64,
    out: Option<PathBuf>,
    against: Option<PathBuf>,
    notes: Option<PathBuf>,
}

fn parse(args: &[String]) -> Option<Args> {
    let mut a = Args {
        dir: PathBuf::new(),
        action: String::new(),
        rest: Vec::new(),
        take: None,
        seconds: None,
        bars: None,
        key: None,
        from: 0.0,
        out: None,
        against: None,
        notes: None,
    };
    let mut positional = Vec::new();
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--take" => {
                let nums: Option<Vec<u32>> =
                    it.next()?.split(',').map(|n| n.trim().parse().ok()).collect();
                a.take = Some(nums?);
            }
            "--seconds" => a.seconds = Some(it.next()?.parse().ok()?),
            "--from" => a.from = it.next()?.parse().ok()?,
            "--key" => a.key = Some(it.next()?.parse().ok()?),
            "--out" => a.out = Some(PathBuf::from(it.next()?)),
            "--against" => a.against = Some(PathBuf::from(it.next()?)),
            "--notes" => a.notes = Some(PathBuf::from(it.next()?)),
            "--bars" => {
                let (lo, hi) = it.next()?.split_once('-')?;
                a.bars = Some((lo.parse().ok()?, hi.parse().ok()?));
            }
            _ if x.starts_with("--") => return None,
            _ => positional.push(x.clone()),
        }
    }
    if let Some(tool @ ("partials" | "model" | "clicks")) = positional.first().map(String::as_str) {
        a.action = tool.into();
        a.rest = positional[1..].to_vec();
        return Some(a);
    }
    if positional.len() < 2 {
        return None;
    }
    a.dir = PathBuf::from(&positional[0]);
    a.action = positional[1].clone();
    a.rest = positional[2..].to_vec();
    Some(a)
}

pub fn run(args: &[String]) -> ExitCode {
    let Some(a) = parse(args) else { return crate::usage() };
    let result = match a.action.as_str() {
        "wav" => wav(&a),
        "describe" => describe(&a),
        "numbers" => numbers(&a),
        "midi" => midi(&a),
        "sheet" => sheet_command(&a),
        "speed" => speed(&a),
        "partials" => partials(&a),
        "clicks" => match a.rest.first() {
            Some(file) => clicks::run(Path::new(file), a.notes.as_deref()),
            None => return crate::usage(),
        },
        "model" => match (a.rest.first(), &a.out) {
            (Some(dir), Some(out)) => model::run(Path::new(dir), out, a.against.as_deref()),
            _ => return crate::usage(),
        },
        _ => return crate::usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::from(2)
        }
    }
}

/// The audio thread's rate and quantum.
const RATE: f64 = wrela_abi::AUDIO_SAMPLE_RATE as f64;
const QUANTUM: u32 = wrela_abi::AUDIO_QUANTUM;

/// The piece, built and loaded, with its take chosen.
fn load(a: &Args) -> Result<CpuHost, String> {
    let out = wrela_driver::build(&a.dir);
    let built = a.dir.join("build").join("audio");
    crate::write_build(&out, &built)?;
    let mut host = CpuHost::load(&built).map_err(|e| e.to_string())?;
    if let Some(t) = &a.take {
        let args: Vec<Value> = t.iter().map(|n| Value::I32(*n as i32)).collect();
        host.call_export("choose", &args).map_err(|e| format!("`choose({t:?})`: {e}"))?;
    }
    Ok(host)
}

/// How many seconds the take sounds: `--seconds`, or the piece's `seconds()`.
fn length(host: &mut CpuHost, a: &Args) -> Result<f64, String> {
    if let Some(s) = a.seconds {
        return Ok(s);
    }
    match host.call_export("seconds", &[]).map_err(|e| format!("`seconds()`: {e}"))?.first() {
        Some(Value::F32(s)) => Ok(f64::from(*s)),
        Some(Value::F64(s)) => Ok(*s),
        _ => Err("`seconds()` returns no f32".into()),
    }
}

/// The take's samples, the channels interleaved, and its length in seconds.
fn render(a: &Args) -> Result<(Vec<f32>, f64), String> {
    let mut host = load(a)?;
    let seconds = length(&mut host, a)?;
    host.frame(0.0, 16, 16).map_err(|e| format!("the first frame: {e}"))?;
    let quanta = (seconds * RATE / f64::from(QUANTUM)).ceil() as u32;
    let samples = host.render_audio(quanta).map_err(|e| e.to_string())?;
    Ok((samples, seconds))
}

fn wav(a: &Args) -> Result<(), String> {
    let out = a.rest.first().ok_or("wav needs a file to write")?;
    let started = std::time::Instant::now();
    let (samples, seconds) = render(a)?;
    let took = started.elapsed().as_secs_f64();
    dsp::write_wav(Path::new(out), &samples, 2).map_err(|e| format!("can't write {out}: {e}"))?;
    let peak = samples.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    println!(
        "{}",
        serde_json::json!({
            "wav": out,
            "seconds": seconds,
            "peak_db": dsp::db(f64::from(peak)),
            "rms_db": dsp::db(dsp::rms(&samples)),
            "clipped": samples.iter().filter(|x| x.abs() > 1.0).count(),
            "build_and_render_seconds": took,
        })
    );
    Ok(())
}

/// A take as `describe()` prints it.
pub(crate) struct Take {
    pub title: String,
    pub name: String,
    pub bars: u32,
    pub beats_per_bar: u32,
    pub seconds: f64,
    pub voices: Vec<String>,
    pub notes: Vec<Played>,
    pub sustain: Vec<(f64, f64)>,
    pub soft: Vec<(f64, f64)>,
    pub beats: Vec<f64>,
}

#[derive(Clone, Copy)]
pub(crate) struct Played {
    pub key: u32,
    pub voice: u32,
    pub onset: f64,
    pub release: f64,
    pub velocity: f64,
    pub bar: u32,
}

fn take_of(a: &Args) -> Result<Take, String> {
    let mut host = load(a)?;
    host.call_export("describe", &[]).map_err(|e| format!("`describe()`: {e}"))?;
    let mut take = Take {
        title: String::new(),
        name: String::new(),
        bars: 0,
        beats_per_bar: 0,
        seconds: 0.0,
        voices: Vec::new(),
        notes: Vec::new(),
        sustain: Vec::new(),
        soft: Vec::new(),
        beats: Vec::new(),
    };
    for line in host.take_logs() {
        let v: serde_json::Value = serde_json::from_str(&line)
            .map_err(|e| format!("`describe()` printed {line:?}: {e}"))?;
        let f = |k: &str| v[k].as_f64().unwrap_or(0.0);
        if v.get("title").is_some() {
            take.title = v["title"].as_str().unwrap_or("").into();
            take.name = v["take"].as_str().unwrap_or("").into();
            take.bars = f("bars") as u32;
            take.beats_per_bar = f("beats_per_bar") as u32;
            take.seconds = f("seconds");
        } else if v.get("name").is_some() {
            take.voices.push(v["name"].as_str().unwrap_or("").into());
        } else if v.get("key").is_some() {
            take.notes.push(Played {
                key: f("key") as u32,
                voice: f("voice") as u32,
                onset: f("onset"),
                release: f("release"),
                velocity: f("velocity"),
                bar: f("bar") as u32,
            });
        } else if v.get("sustain").is_some() {
            take.sustain.push((f("sustain"), f("depth")));
        } else if v.get("soft").is_some() {
            take.soft.push((f("soft"), f("depth")));
        } else if v.get("beat").is_some() {
            take.beats.push(f("beat"));
        }
    }
    Ok(take)
}

fn describe(a: &Args) -> Result<(), String> {
    let t = take_of(a)?;
    let notes: Vec<_> = t
        .notes
        .iter()
        .map(|n| {
            serde_json::json!({
                "key": n.key, "voice": t.voices.get(n.voice as usize), "bar": n.bar,
                "onset": n.onset, "release": n.release, "velocity": n.velocity,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "title": t.title, "take": t.name, "bars": t.bars, "seconds": t.seconds,
            "voices": t.voices, "notes": notes,
            "sustain": t.sustain, "soft": t.soft, "beats": t.beats,
        }))
        .expect("json")
    );
    Ok(())
}

/// The take in numbers: per bar, its tempo, its loudness and each voice's mean velocity; and
/// the whole take's level.
fn numbers(a: &Args) -> Result<(), String> {
    let t = take_of(a)?;
    let (samples, _) = render(a)?;
    let per = t.beats_per_bar as usize;
    let mut bars = Vec::new();
    for b in 0..t.bars as usize {
        let (Some(&start), Some(&end)) = (t.beats.get(b * per), t.beats.get((b + 1) * per)) else {
            break;
        };
        let bpm = 60.0 * per as f64 / (end - start);
        let window = &samples
            [(2.0 * start * RATE) as usize..((2.0 * end * RATE) as usize).min(samples.len())];
        let mut voices = serde_json::Map::new();
        for (i, name) in t.voices.iter().enumerate() {
            let v: Vec<f64> = t
                .notes
                .iter()
                .filter(|n| n.bar as usize == b + 1 && n.voice as usize == i)
                .map(|n| n.velocity)
                .collect();
            if !v.is_empty() {
                voices.insert(
                    name.clone(),
                    serde_json::json!(round(v.iter().sum::<f64>() / v.len() as f64, 3)),
                );
            }
        }
        bars.push(serde_json::json!({
            "bar": b + 1,
            "start": round(start, 3),
            "bpm": round(bpm, 1),
            "rms_db": round(dsp::db(dsp::rms(window)), 1),
            "velocity": voices,
        }));
    }
    let peak = samples.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "title": t.title, "take": t.name, "seconds": round(t.seconds, 2),
            "notes": t.notes.len(),
            "peak_db": round(dsp::db(f64::from(peak)), 1),
            "rms_db": round(dsp::db(dsp::rms(&samples)), 1),
            "bars": bars,
        }))
        .expect("json")
    );
    Ok(())
}

fn round(x: f64, places: i32) -> f64 {
    let s = 10f64.powi(places);
    (x * s).round() / s
}

/// The take as a standard MIDI file: one track at 960 ticks a second (a quarter note a second),
/// its notes, and the pedals as controllers 64 (sustain) and 67 (soft).
fn midi(a: &Args) -> Result<(), String> {
    let out = a.rest.first().ok_or("midi needs a file to write")?;
    let t = take_of(a)?;
    let mut events: Vec<(u64, u8, [u8; 3])> = Vec::new();
    let tick = |s: f64| (s.max(0.0) * 960.0).round() as u64;
    for n in &t.notes {
        let v = ((n.velocity * 127.0).round() as u8).clamp(1, 127);
        // Offs sort before ons at the same tick.
        events.push((tick(n.onset), 1, [0x90, n.key as u8, v]));
        events.push((tick(n.release), 0, [0x80, n.key as u8, 0]));
    }
    for (at, depth) in &t.sustain {
        events.push((tick(*at), 0, [0xB0, 64, (depth * 127.0).round() as u8]));
    }
    for (at, depth) in &t.soft {
        events.push((tick(*at), 0, [0xB0, 67, (depth * 127.0).round() as u8]));
    }
    events.sort_by_key(|e| (e.0, e.1));
    let mut track = Vec::new();
    // Tempo: a quarter note a second.
    track.extend_from_slice(&[0x00, 0xFF, 0x51, 0x03, 0x0F, 0x42, 0x40]);
    let mut last = 0;
    for (at, _, bytes) in events {
        dsp::vlq(&mut track, at - last);
        track.extend_from_slice(&bytes);
        last = at;
    }
    track.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
    let mut file = b"MThd\0\0\0\x06\0\0\0\x01".to_vec();
    file.extend_from_slice(&960u16.to_be_bytes());
    file.extend_from_slice(b"MTrk");
    file.extend_from_slice(&(track.len() as u32).to_be_bytes());
    file.extend_from_slice(&track);
    std::fs::write(out, file).map_err(|e| format!("can't write {out}: {e}"))?;
    println!("{}", serde_json::json!({ "midi": out, "notes": t.notes.len() }));
    Ok(())
}

fn sheet_command(a: &Args) -> Result<(), String> {
    let out = a.rest.first().ok_or("sheet needs a PNG to write")?;
    let t = take_of(a)?;
    let (samples, _) = render(a)?;
    let png = sheet::draw(&t, &samples, a.bars)?;
    std::fs::write(out, png).map_err(|e| format!("can't write {out}: {e}"))?;
    println!("{}", serde_json::json!({ "sheet": out }));
    Ok(())
}

/// How fast the voice renders on the native host: each quantum timed.
fn speed(a: &Args) -> Result<(), String> {
    let mut host = load(a)?;
    let seconds = length(&mut host, a)?;
    host.frame(0.0, 16, 16).map_err(|e| format!("the first frame: {e}"))?;
    let quanta = (seconds * RATE / f64::from(QUANTUM)).ceil() as u32;
    let mut times = Vec::with_capacity(quanta as usize);
    for _ in 0..quanta {
        let started = std::time::Instant::now();
        host.render_audio(1).map_err(|e| e.to_string())?;
        times.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let total: f64 = times.iter().sum();
    let mut sorted = times.clone();
    sorted.sort_by(f64::total_cmp);
    let budget = 1000.0 * f64::from(QUANTUM) / RATE;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "seconds": seconds,
            "quanta": quanta,
            "render_seconds": round(total / 1000.0, 3),
            "times_real_time": round(seconds / (total / 1000.0), 2),
            "quantum_budget_ms": round(budget, 3),
            "mean_ms": round(total / f64::from(quanta), 4),
            "p99_ms": round(sorted[(sorted.len() * 99 / 100).min(sorted.len() - 1)], 4),
            "worst_ms": round(*sorted.last().unwrap_or(&0.0), 4),
        }))
        .expect("json")
    );
    Ok(())
}

/// A note's partials in a WAV file (a recorded note, say): each one's frequency, its level
/// over time, and its decay, for fitting a piano's model to a recorded piano.
fn partials(a: &Args) -> Result<(), String> {
    let file = a.rest.first().ok_or("partials needs a WAV file")?;
    let key = a.key.ok_or("partials needs --key, the note's MIDI key")?;
    let (samples, channels, rate) = dsp::read_wav(Path::new(file))?;
    let mono: Vec<f32> =
        samples.chunks_exact(channels).map(|c| c.iter().sum::<f32>() / channels as f32).collect();
    let report = measure::report(&mono, rate, key, a.from, a.seconds.unwrap_or(8.0));
    println!("{}", serde_json::to_string_pretty(&report).expect("json"));
    Ok(())
}
