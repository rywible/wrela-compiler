//! `wrela-host`: runs a build headless.
//!
//! - `wrela-host <build-dir> [--frames N] [--fps F] [--size WxH] [--input script.json]
//!   [--png out.png] [--rgba out.rgba] [--timestamps] [--log out.ticks] [--workers N]`: frames
//!   on the GPU (in lockstep with the program's ticker, if it has one), then the state hash.
//! - `wrela-host --no-gpu <build-dir> --ticks N [--input script.json] [--log out.ticks]
//!   [--workers N]`: the program's ticks alone, with no GPU and no frames: each tick's state
//!   hash.
//! - `wrela-host --replay <log> [--no-gpu] <build-dir> [--workers N]`: a tick log's ticks,
//!   each with its records, checking each state hash; it fails at the first that differs.
//!
//! A build of this binary without the `gpu` feature has the last two only (an x86-64 build run
//! under Rosetta, say, which checks the sim's bits on x86's code generation).

use std::path::PathBuf;
use std::process::ExitCode;
use wrela_host::{CpuBuild, TickLog};

const USAGE: &str = "usage:
  wrela-host <build-dir> [--frames N] [--fps F] [--size WxH] [--input script.json] [--png out.png] [--rgba out.rgba] [--timestamps] [--log out.ticks] [--workers N]
  wrela-host --no-gpu <build-dir> --ticks N [--input script.json] [--log out.ticks] [--workers N]
  wrela-host --replay <log> [--no-gpu] <build-dir> [--workers N]

With a GPU: runs the build's frame(i / fps, width, height) for i in 0..N (default 1 frame at 60
fps, 640x360), its ticker (if it starts one) in lockstep, then prints the state hash: FNV-1a 64
over every submitted byte, 16 hex digits. --input gives the program a script of input events
(runtime/abi `input`). --png and --rgba write the last frame (raw RGBA8, rows top to bottom).
--timestamps prints each dispatch's and screen pass's GPU time to stderr.

--no-gpu runs the ticker's ticks alone, with no GPU and no frames, and prints the first world's
state hash and each tick's (`tick K HASH`). --log writes the ticks as a tick log (runtime/abi
`ticks`). --replay runs a tick log's ticks with their records, checks each state hash, and fails
at the first that differs, naming it. --workers sets the threads that run parallel work (the
program's own among them).";

struct Args {
    dir: PathBuf,
    frames: u32,
    fps: f64,
    size: (u32, u32),
    png: Option<PathBuf>,
    rgba: Option<PathBuf>,
    input: Option<PathBuf>,
    timestamps: bool,
    no_gpu: bool,
    ticks: Option<u32>,
    log: Option<PathBuf>,
    replay: Option<PathBuf>,
    workers: u32,
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
        input: None,
        timestamps: false,
        no_gpu: false,
        ticks: None,
        log: None,
        replay: None,
        workers: 0,
    };
    while let Some(arg) = argv.next() {
        let mut value = |flag: &str| argv.next().ok_or_else(|| format!("{flag} needs a value"));
        let whole = |flag: &str, v: String| -> Result<u32, String> {
            v.parse().map_err(|_| format!("{flag} needs a whole number"))
        };
        match arg.as_str() {
            "--frames" => args.frames = whole("--frames", value("--frames")?)?,
            "--ticks" => args.ticks = Some(whole("--ticks", value("--ticks")?)?),
            "--workers" => args.workers = whole("--workers", value("--workers")?)?,
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
            "--input" => args.input = Some(value("--input")?.into()),
            "--log" => args.log = Some(value("--log")?.into()),
            "--replay" => args.replay = Some(value("--replay")?.into()),
            "--timestamps" => args.timestamps = true,
            "--no-gpu" => args.no_gpu = true,
            "-h" | "--help" => return Err(USAGE.into()),
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n\n{USAGE}"));
            }
            _ if dir.is_none() => dir = Some(PathBuf::from(arg)),
            _ => return Err(format!("unexpected argument {arg:?}\n\n{USAGE}")),
        }
    }
    args.dir = dir.ok_or_else(|| USAGE.to_string())?;
    if args.no_gpu && args.replay.is_none() && args.ticks.is_none() {
        return Err(format!("--no-gpu needs --ticks N\n\n{USAGE}"));
    }
    Ok(args)
}

/// What went wrong: the host's error, or a replay that didn't match.
enum Failure {
    Host(wrela_host::Error),
    Replay(wrela_host::ReplayError),
}

impl From<wrela_host::Error> for Failure {
    fn from(e: wrela_host::Error) -> Failure {
        Failure::Host(e)
    }
}

fn read_script(args: &Args) -> Result<Vec<wrela_host::Scripted>, wrela_host::Error> {
    match &args.input {
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| wrela_host::Error::io(path, e))?;
            wrela_host::parse_script(&text)
                .map_err(|why| wrela_host::Error::Program(format!("{}: {why}", path.display())))
        }
        None => Ok(Vec::new()),
    }
}

fn write_log(path: &PathBuf, log: &TickLog) -> Result<(), wrela_host::Error> {
    std::fs::write(path, log.encode()).map_err(|e| wrela_host::Error::io(path, e))
}

/// `--replay`: a tick log's ticks, each hash checked.
fn replay(args: &Args, path: &PathBuf) -> Result<(), Failure> {
    let bytes = std::fs::read(path).map_err(|e| wrela_host::Error::io(path, e))?;
    let log = TickLog::decode(&bytes)
        .map_err(|e| wrela_host::Error::Program(format!("{}: {e}", path.display())))?;
    let built = CpuBuild::load(&args.dir)?;
    let n = built.replay(&log, args.workers).map_err(Failure::Replay)?;
    println!("{n} ticks replayed: every state hash is the log's");
    Ok(())
}

/// `--no-gpu`: the ticks alone.
fn ticks_alone(args: &Args) -> Result<(), Failure> {
    let script = read_script(args)?;
    let built = CpuBuild::load(&args.dir)?;
    let log = built.record_ticks(args.ticks.unwrap_or(0), &script, args.workers)?;
    println!("first {}", wrela_abi::hash::hex(log.first));
    for (k, t) in log.ticks.iter().enumerate() {
        println!("tick {k} {}", wrela_abi::hash::hex(t.hash));
    }
    if let Some(path) = &args.log {
        write_log(path, &log)?;
    }
    Ok(())
}

#[cfg(feature = "gpu")]
fn frames(args: &Args) -> Result<(), Failure> {
    use wrela_host::{Host, Options};
    let script = read_script(args)?;
    let options = Options {
        timestamps: args.timestamps,
        workers: args.workers,
        defer_init: true,
        ..Options::default()
    };
    let mut host = Host::load_with(&args.dir, &options)?;
    let logging = args.log.is_some();
    host.want_hashes(logging);
    host.init()?;
    let (w, h) = args.size;
    let (run, log) = host.run_lockstep(args.frames, args.fps, w, h, &script, logging)?;
    drop(host); // release the GPU (and its lock) before writing files
    if let Some(path) = &args.png {
        run.write_png(path)?;
    }
    if let Some(path) = &args.rgba {
        std::fs::write(path, &run.frame).map_err(|e| wrela_host::Error::io(path, e))?;
    }
    if let (Some(path), Some(log)) = (&args.log, &log) {
        write_log(path, log)?;
    }
    for t in &run.timings {
        eprintln!("frame {:>4}  {:<24} {:>10.3} us", t.frame, t.label, t.nanos / 1000.0);
    }
    println!("{}", run.hash_hex());
    Ok(())
}

#[cfg(not(feature = "gpu"))]
fn frames(_: &Args) -> Result<(), Failure> {
    Err(Failure::Host(wrela_host::Error::Gpu(
        "this wrela-host was built without the `gpu` feature: run ticks alone with --no-gpu, or --replay".into(),
    )))
}

fn run(args: &Args) -> Result<(), Failure> {
    if let Some(path) = &args.replay {
        return replay(args, path);
    }
    if args.no_gpu {
        return ticks_alone(args);
    }
    frames(args)
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
        Err(Failure::Host(e)) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
        Err(Failure::Replay(e)) => {
            eprintln!("the replay failed: {e}");
            ExitCode::FAILURE
        }
    }
}
