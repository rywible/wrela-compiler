//! `wrela-host <build-dir> [--frames N] [--fps F] [--size WxH] [--png out.png] [--rgba out.rgba]
//! [--timestamps]`: runs a build headless and prints its state hash.

use std::path::PathBuf;
use std::process::ExitCode;
use wrela_host::{Host, Options, frame_time};

const USAGE: &str = "usage: wrela-host <build-dir> [--frames N] [--fps F] [--size WxH] [--png out.png] [--rgba out.rgba] [--timestamps]

Runs the build's frame(i / fps, width, height) for i in 0..N (default 1 frame at 60 fps,
640x360), then prints the state hash: FNV-1a 64 over every submitted byte, 16 hex digits.
--png and --rgba write the last frame (raw RGBA8, rows top to bottom). --timestamps prints each
dispatch's and screen pass's GPU time to stderr.";

struct Args {
    dir: PathBuf,
    frames: u32,
    fps: f64,
    size: (u32, u32),
    png: Option<PathBuf>,
    rgba: Option<PathBuf>,
    timestamps: bool,
}

fn parse(mut argv: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut dir = None;
    let mut args = Args {
        dir: PathBuf::new(),
        frames: 1,
        fps: 60.0,
        size: (640, 360),
        png: None,
        rgba: None,
        timestamps: false,
    };
    while let Some(arg) = argv.next() {
        let mut value = |flag: &str| argv.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--frames" => {
                args.frames = value("--frames")?
                    .parse()
                    .map_err(|_| "--frames needs a whole number".to_string())?;
            }
            "--fps" => {
                args.fps =
                    value("--fps")?.parse().map_err(|_| "--fps needs a number".to_string())?;
                if !(args.fps.is_finite() && args.fps > 0.0) {
                    return Err("--fps must be positive".into());
                }
            }
            "--size" => {
                let v = value("--size")?;
                let parsed =
                    v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
                args.size =
                    parsed.ok_or_else(|| format!("--size needs WIDTHxHEIGHT, not {v:?}"))?;
            }
            "--png" => args.png = Some(value("--png")?.into()),
            "--rgba" => args.rgba = Some(value("--rgba")?.into()),
            "--timestamps" => args.timestamps = true,
            "-h" | "--help" => return Err(USAGE.into()),
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n\n{USAGE}"));
            }
            _ if dir.is_none() => dir = Some(PathBuf::from(arg)),
            _ => return Err(format!("unexpected argument {arg:?}\n\n{USAGE}")),
        }
    }
    args.dir = dir.ok_or_else(|| USAGE.to_string())?;
    Ok(args)
}

fn run(args: &Args) -> Result<(), wrela_host::Error> {
    let mut host =
        Host::load_with(&args.dir, &Options { timestamps: args.timestamps, ..Options::default() })?;
    let times: Vec<f32> = (0..args.frames).map(|i| frame_time(i, args.fps)).collect();
    let run = host.run_frames(&times, args.size.0, args.size.1)?;
    drop(host); // release the GPU (and its lock) before writing files
    if let Some(path) = &args.png {
        run.write_png(path)?;
    }
    if let Some(path) = &args.rgba {
        std::fs::write(path, &run.frame)
            .map_err(|e| wrela_host::Error::Io { path: path.clone(), source: e })?;
    }
    for t in &run.timings {
        eprintln!("frame {:>4}  {:<24} {:>10.3} us", t.frame, t.label, t.nanos / 1000.0);
    }
    println!("{}", run.hash_hex());
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
