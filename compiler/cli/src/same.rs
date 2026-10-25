//! `wrela same <package-dir> --against <build-dir> [--frames n] [--fps f] [--size WxH]
//! [--input <script.json>] [--within x] [--no-gpu] [--json]`: whether the package, built now,
//! behaves as a build of it saved before does (language.md §23.9). An agent runs it after a
//! refactor, a std change, or before moving a pin.
//!
//! Two builds are the same when every command byte each submits is the same, frame by frame
//! (the hosts' state hash after each frame, FNV-1a 64 of every byte submitted so far), their
//! last screens are within `within` of each other (a mean over every channel in 8-bit steps, as
//! a golden is compared; GPU results differ a little between runs), and they reach the same
//! pipelines. Each runs from `init`, on one worker (more can move work between frames), with
//! its own empty storage and the same input script, in lockstep with its ticker as `wrela
//! trace` runs frames, and frames are numbered from 0. The candidate is built as `wrela build`
//! builds (a release build), into `<package>/build/same`; `--against` must be such a build, made
//! outside it (`wrela build -o <dir>`), and is never written. Without a GPU (`--no-gpu`, or no
//! pipelines), the commands are checked but not run, so the screens aren't compared.
//!
//! Exit status: 0 the same, 1 different, 2 a usage error or a build that won't load.

use crate::trace::Runner;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wrela_host::{CpuBuild, Host, Options};

struct Args {
    dir: PathBuf,
    against: PathBuf,
    frames: u32,
    fps: f64,
    size: (u32, u32),
    input: Option<PathBuf>,
    within: f64,
    no_gpu: bool,
    json: bool,
}

fn parse(args: &[String]) -> Option<Args> {
    let mut a = Args {
        dir: PathBuf::new(),
        against: PathBuf::new(),
        frames: 60,
        fps: 60.0,
        size: (640, 480),
        input: None,
        // A golden's default tolerance (language.md §13.2).
        within: 0.5,
        no_gpu: false,
        json: false,
    };
    let (mut dir, mut against) = (None, None);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--against" => against = Some(PathBuf::from(it.next()?)),
            "--frames" => a.frames = it.next()?.parse().ok().filter(|&n| n > 0)?,
            "--fps" => a.fps = it.next()?.parse().ok().filter(|&f: &f64| f > 0.0)?,
            "--size" => a.size = crate::parse_size(it.next()?)?,
            "--input" => a.input = Some(PathBuf::from(it.next()?)),
            "--within" => {
                a.within = it.next()?.parse().ok().filter(|&w: &f64| (0.0..=255.0).contains(&w))?
            }
            "--no-gpu" => a.no_gpu = true,
            "--json" => a.json = true,
            s if !s.starts_with('-') && dir.is_none() => dir = Some(PathBuf::from(s)),
            _ => return None,
        }
    }
    a.dir = dir?;
    a.against = against?;
    Some(a)
}

pub fn run(args: &[String]) -> ExitCode {
    let Some(a) = parse(args) else { return crate::usage() };
    match same(&a) {
        Ok(report) => {
            if a.json {
                println!("{}", serde_json::to_string_pretty(&report.json()).unwrap_or_default());
            } else {
                println!("{}", report.text());
            }
            if report.same() { ExitCode::SUCCESS } else { ExitCode::from(1) }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

/// A run of one build: its state hash after each frame, and its last screen (on the GPU).
struct Run {
    hashes: Vec<u64>,
    screen: Option<Vec<u8>>,
}

/// What `same` found.
struct Report {
    frames: u32,
    /// The first frame after which the builds' commands differ.
    first_differing: Option<u32>,
    screen: Option<wrela_host::image::FrameDiff>,
    within: f64,
    added: Vec<String>,
    removed: Vec<String>,
    against: (PathBuf, String),
    candidate: (PathBuf, String),
}

impl Report {
    fn same(&self) -> bool {
        self.first_differing.is_none()
            && self.screen.is_none_or(|d| d.mean <= self.within)
            && self.added.is_empty()
            && self.removed.is_empty()
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "same": self.same(),
            "frames": self.frames,
            "first_differing_frame": self.first_differing,
            "screen": self.screen.map(|d| serde_json::json!({
                "mean": d.mean, "max": d.max, "differing": d.differing, "channels": d.channels,
                "within": self.within,
            })),
            "pipelines": { "added": self.added, "removed": self.removed },
            "against": { "dir": self.against.0.display().to_string(), "hash": self.against.1 },
            "candidate": { "dir": self.candidate.0.display().to_string(), "hash": self.candidate.1 },
        })
    }

    fn text(&self) -> String {
        let against = self.against.0.display();
        let screen = match self.screen {
            Some(d) if d.mean <= self.within => {
                format!("last frame within {:.2} (of {})", d.mean, self.within)
            }
            Some(d) => format!(
                "last frame {:.2} from it (max {}, {:.0}% of channels)",
                d.mean,
                d.max,
                100.0 * d.differing as f64 / d.channels.max(1) as f64
            ),
            None => "screens not compared (no GPU run)".to_string(),
        };
        let commands = match self.first_differing {
            None => format!("{} frames, the same commands", self.frames),
            Some(i) => format!("commands differ from frame {i}"),
        };
        let pipelines = if self.added.is_empty() && self.removed.is_empty() {
            "the same pipelines".to_string()
        } else {
            let mut parts: Vec<String> = self.added.iter().map(|p| format!("+{p}")).collect();
            parts.extend(self.removed.iter().map(|p| format!("-{p}")));
            format!("pipelines: {}", parts.join(" "))
        };
        let verdict = if self.same() { "same as" } else { "differs from" };
        format!("{verdict} {against}: {commands}; {screen}; {pipelines}")
    }
}

fn same(a: &Args) -> Result<Report, String> {
    // Before anything is written: the saved build must be one, and the candidate's directory
    // must be neither it nor inside it, nor around it.
    let against = a
        .against
        .canonicalize()
        .map_err(|e| format!("can't read the build `{}`: {e}", a.against.display()))?;
    if crate::profile::overwritten_by(&a.dir, "same", &against)? {
        return Err(format!(
            "`{}` would be overwritten by the build to compare; save it outside `build/same` with `wrela build -o <dir>`",
            a.against.display()
        ));
    }
    let manifest = |dir: &Path| -> Result<wrela_abi::Manifest, String> {
        let text = std::fs::read_to_string(dir.join("manifest.json"))
            .map_err(|e| format!("`{}` isn't a build: {e}", dir.display()))?;
        wrela_abi::Manifest::parse(&text).map_err(|e| format!("`{}`: {e}", dir.display()))
    };
    let before = manifest(&against)?;
    let script = crate::read_script(a.input.as_deref())?;
    // The candidate, as `wrela build` builds it.
    let built = crate::build_into(&a.dir, "same", None)?;
    let after = manifest(&built)?;
    let names = |m: &wrela_abi::Manifest| -> Vec<String> {
        let mut v: Vec<String> = m.pipelines.iter().map(|p| p.name.clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    let (was, now) = (names(&before), names(&after));
    let gpu = !a.no_gpu && !(was.is_empty() && now.is_empty());
    let first = run_build(&against, a, &script, gpu)?;
    let second = run_build(&built, a, &script, gpu)?;
    let first_differing =
        (0..a.frames).find(|&i| first.hashes.get(i as usize) != second.hashes.get(i as usize));
    let screen = match (&first.screen, &second.screen) {
        (Some(x), Some(y)) => Some(wrela_host::image::compare(x, y)?),
        _ => None,
    };
    let hash = |dir: &Path| crate::profile::build_hash(dir).unwrap_or_default();
    Ok(Report {
        frames: a.frames,
        first_differing,
        screen,
        within: a.within,
        added: now.iter().filter(|p| !was.contains(p)).cloned().collect(),
        removed: was.iter().filter(|p| !now.contains(p)).cloned().collect(),
        against: (a.against.clone(), hash(&against)),
        candidate: (built.clone(), hash(&built)),
    })
}

/// Runs the build in `dir` for the frames, from `init`, with storage of its own.
fn run_build(
    dir: &Path,
    a: &Args,
    script: &[wrela_abi::input::Scripted],
    gpu: bool,
) -> Result<Run, String> {
    let storage = crate::profile::Storage::new()?;
    let what = |e: &dyn std::fmt::Display| format!("`{}`: {e}", dir.display());
    let mut runner = if gpu {
        let options =
            Options { workers: 1, storage: Some(storage.0.clone()), ..Options::default() };
        Runner::Gpu(Box::new(Host::load_with(dir, &options).map_err(|e| what(&e))?))
    } else {
        let build = CpuBuild::load_cpu(dir).map_err(|e| what(&e))?;
        let mut host = build.instantiate_in(1, Some(&storage.0)).map_err(|e| what(&e))?;
        host.init().map_err(|e| what(&e))?;
        Runner::Cpu(Box::new(host))
    };
    let mut hashes = Vec::with_capacity(a.frames as usize);
    for i in 0..a.frames {
        runner.frame(i, a.fps, a.size, script).map_err(|e| what(&format!("frame {i}: {e}")))?;
        hashes.push(runner.hash());
    }
    let screen = match &mut runner {
        Runner::Gpu(host) => Some(host.read_screen().map_err(|e| what(&e))?),
        Runner::Cpu(_) => None,
    };
    Ok(Run { hashes, screen })
}
