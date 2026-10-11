//! `wrela estimate <package-dir> [--frames n] [--skip n] [--size WxH] [--input script.json]
//! [--json] [--work out.json] [--pair m4-profile.json]`: a frame's GPU time on the reference
//! device (vision.md: the MacBook Air M4), estimated on a machine without its GPU. It builds the
//! program as `wrela build` does, runs its frames on the native host (in lockstep with its
//! ticker, 60 a second; 120 frames, the first 20 not counted, unless said) on lavapipe, Mesa's
//! Vulkan on the CPU, and reports each label's estimated time, the frame's, and the frame rate
//! that gives.
//!
//! How (a cost model, not a simulation of the M4):
//! - Each pass and dispatch runs alone (the serial timing mode), timed by lavapipe and counted by
//!   its pipeline statistics: vertex and fragment shader invocations, triangles, compute
//!   invocations, and the bytes its targets move ([`wrela_host::GpuWork`]). The counts depend on
//!   the commands, not on the GPU; lavapipe's time stands for what the counts can't see, such as
//!   a shader's loops and texture reads.
//! - A label's M4 time is a weighted sum of those features, the weights fitted (least squares,
//!   none negative) to the M4 times recorded in `estimate/calibration.json`, each beside the same
//!   frames' features on lavapipe. The fit runs each time, so the report gives its own error:
//!   each calibration point predicted by a fit without it.
//! - A frame's estimate is the sum of its passes' and dispatches' estimates, as the serial mode
//!   measures it on the M4. Chrome's frame (vision.md's budget) adds what a pass's time doesn't
//!   show, such as vertex work the M4 does before a pass's first timestamp; the report gives it
//!   as a range, between the smallest and largest of the ratios recorded (calibration.json's
//!   `chrome`, which says why they differ).
//!
//! Lavapipe's time depends on the CPU and how many threads it uses (`LP_NUM_THREADS`, all the
//! cores by default): the calibration names the machine it was taken on, and the report says
//! when this one differs. Nothing else should run on the machine meanwhile.
//!
//! `--work` writes each counted frame's features by label (JSON). `--pair` takes an M4 profile
//! of the same frames (`wrela profile --serial --json` on the M4, with the same `--frames`,
//! `--size` and `--input`, and `--skip 0` here) and prints calibration points instead of a
//! report: each label's M4 median beside its typical features here, for calibration.json.

use crate::profile::{Baseline, percentile};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use wrela_host::{GpuTiming, GpuWork, Options, Timing};

struct Args {
    dir: PathBuf,
    frames: u32,
    skip: u32,
    size: (u32, u32),
    input: Option<PathBuf>,
    json: bool,
    work: Option<PathBuf>,
    pair: Option<PathBuf>,
}

fn parse(args: &[String]) -> Option<Args> {
    let mut a = Args {
        dir: PathBuf::new(),
        frames: 120,
        skip: 20,
        size: (1920, 1080),
        input: None,
        json: false,
        work: None,
        pair: None,
    };
    let mut dir = None;
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--frames" => a.frames = it.next()?.parse().ok().filter(|&n| n > 0)?,
            "--skip" => a.skip = it.next()?.parse().ok()?,
            "--size" => a.size = crate::parse_size(it.next()?)?,
            "--input" => a.input = Some(PathBuf::from(it.next()?)),
            "--json" => a.json = true,
            "--work" => a.work = Some(PathBuf::from(it.next()?)),
            "--pair" => a.pair = Some(PathBuf::from(it.next()?)),
            _ if dir.is_none() && !x.starts_with('-') => dir = Some(PathBuf::from(x)),
            _ => return None,
        }
    }
    a.dir = dir?;
    (a.skip < a.frames).then_some(a)
}

pub fn run(args: &[String]) -> ExitCode {
    let Some(a) = parse(args) else { return crate::usage() };
    let calibration = match Calibration::parse(CALIBRATION) {
        Ok(c) => c,
        Err(why) => {
            eprintln!("error: estimate/calibration.json: {why}");
            return ExitCode::from(2);
        }
    };
    // The profile is read first, so a wrong path doesn't wait for a run.
    let pair = match a.pair.as_deref().map(read_profile).transpose() {
        Ok(p) => p,
        Err(why) => {
            eprintln!("error: {why}");
            return ExitCode::from(2);
        }
    };
    match measure(&a) {
        Ok(m) if pair.is_some() => {
            let points = pair_points(&a, &m, pair.as_ref().expect("checked"));
            println!("{}", serde_json::to_string_pretty(&points).expect("JSON"));
            ExitCode::SUCCESS
        }
        Ok(m) => {
            let report = Report::new(&calibration, &m);
            if let Some(path) = &a.work
                && let Err(e) = std::fs::write(path, work_json(&a, &m).to_string())
            {
                eprintln!("error: can't write {}: {e}", path.display());
                return ExitCode::from(2);
            }
            if a.json {
                println!("{}", report.json(&a, &m));
            } else {
                print!("{}", report.text(&a, &m));
            }
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::from(2)
        }
    }
}

/// What the model reads of a pass or dispatch, or of a label's in a frame (their sums).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Features {
    /// Passes and dispatches.
    items: f64,
    /// Lavapipe's time, ms.
    lvp_ms: f64,
    /// Millions of vertex shader invocations, of triangles rasterized, of fragment shader
    /// invocations and of compute invocations.
    vertices: f64,
    primitives: f64,
    fragments: f64,
    invocations: f64,
    /// MB its passes' targets moved.
    target_mb: f64,
}

/// The features' names, in [`Features::get`]'s order.
const FEATURES: [&str; 7] =
    ["items", "lvp_ms", "vertices", "primitives", "fragments", "invocations", "target_mb"];

impl Features {
    fn of(t: &GpuTiming, w: &GpuWork) -> Features {
        let sum = |f: fn(&wrela_host::GpuPiece) -> u64| {
            w.pieces.iter().map(|p| f(p) as f64).sum::<f64>() / 1e6
        };
        Features {
            items: 1.0,
            lvp_ms: t.nanos / 1e6,
            vertices: sum(|p| p.vertices),
            primitives: sum(|p| p.primitives),
            fragments: sum(|p| p.fragments),
            invocations: sum(|p| p.invocations),
            target_mb: w.target_bytes as f64 / 1e6,
        }
    }

    fn get(&self, i: usize) -> f64 {
        [
            self.items,
            self.lvp_ms,
            self.vertices,
            self.primitives,
            self.fragments,
            self.invocations,
            self.target_mb,
        ][i]
    }

    fn from_values(v: &[f64; 7]) -> Features {
        Features {
            items: v[0],
            lvp_ms: v[1],
            vertices: v[2],
            primitives: v[3],
            fragments: v[4],
            invocations: v[5],
            target_mb: v[6],
        }
    }

    fn values(&self) -> [f64; 7] {
        std::array::from_fn(|i| self.get(i))
    }

    fn add(&mut self, o: &Features) {
        *self = Features::from_values(&std::array::from_fn(|i| self.get(i) + o.get(i)));
    }

    fn json(&self) -> serde_json::Value {
        FEATURES
            .iter()
            .enumerate()
            .map(|(i, n)| ((*n).to_string(), serde_json::Value::from(self.get(i))))
            .collect()
    }

    fn parse(v: &serde_json::Value) -> Result<Features, String> {
        let mut out = [0.0; 7];
        for (i, n) in FEATURES.iter().enumerate() {
            out[i] = v[*n].as_f64().ok_or_else(|| format!("no `{n}` in {v}"))?;
        }
        Ok(Features::from_values(&out))
    }
}

/// The features of the frames' typical work: of the frames ordered by lavapipe's time, the mean
/// of the middle half's (the median frame's, less noisy). Linear in the features, so the model's
/// estimate of it is the estimate of that mean.
pub(crate) fn typical(frames: &[Features]) -> Features {
    let mut f = frames.to_vec();
    f.sort_by(|a, b| a.lvp_ms.total_cmp(&b.lvp_ms));
    let (from, to) = (f.len() / 4, (f.len() * 3).div_ceil(4).max(f.len() / 4 + 1).min(f.len()));
    let middle = &f[from..to];
    let mut sum = Features::default();
    for x in middle {
        sum.add(x);
    }
    let n = middle.len().max(1) as f64;
    Features::from_values(&sum.values().map(|v| v / n))
}

/// What a run measured: each counted frame's features by label, in the order labels first ran.
struct Measured {
    adapter: String,
    labels: Vec<String>,
    /// For each counted frame, each label's features (absent where it didn't run).
    frames: Vec<BTreeMap<usize, Features>>,
}

fn measure(a: &Args) -> Result<Measured, String> {
    let options = Options {
        timing: Timing::Serial,
        counts: true,
        quiet: true,
        defer_init: true,
        ..Options::default()
    };
    let (mut host, script, _) =
        crate::profile::load(&a.dir, "estimate", a.input.as_deref(), &options)?;
    let adapter = host.adapter();
    host.init().map_err(|e| format!("in `init`: {e}"))?;
    let (w, h) = a.size;
    let mut labels: Vec<String> = Vec::new();
    let mut frames = Vec::new();
    for k in 0..a.frames {
        host.lockstep_frame(k, 60.0, w, h, &script).map_err(|e| format!("in frame {k}: {e}"))?;
        let timings = host.take_timings().map_err(|e| e.to_string())?;
        let work = host.take_work().map_err(|e| e.to_string())?;
        if k < a.skip {
            continue;
        }
        let mut frame: BTreeMap<usize, Features> = BTreeMap::new();
        for (t, w) in timings.iter().zip(&work) {
            let i = match labels.iter().position(|l| *l == t.label) {
                Some(i) => i,
                None => {
                    labels.push(t.label.clone());
                    labels.len() - 1
                }
            };
            frame.entry(i).or_default().add(&Features::of(t, w));
        }
        frames.push(frame);
    }
    Ok(Measured { adapter, labels, frames })
}

/// `--work`'s JSON: how the run ran, and each counted frame's features by label.
fn work_json(a: &Args, m: &Measured) -> serde_json::Value {
    let frames: Vec<serde_json::Value> = m
        .frames
        .iter()
        .map(|f| f.iter().map(|(i, x)| (m.labels[*i].clone(), x.json())).collect())
        .collect();
    serde_json::json!({
        "package": a.dir.display().to_string(),
        "size": [a.size.0, a.size.1],
        "frames": a.frames,
        "skip": a.skip,
        "adapter": m.adapter,
        "machine": machine(),
        "work": frames,
    })
}

/// An M4 profile (`wrela profile --serial --json`): each label's median ms, and how it ran.
fn read_profile(path: &std::path::Path) -> Result<Baseline, String> {
    let p = Baseline::read(path)?;
    if !p.serial {
        return Err(format!("{} wasn't run with --serial", path.display()));
    }
    Ok(p)
}

/// Calibration points: each label the profile and this run share, run in most frames here.
fn pair_points(a: &Args, m: &Measured, p: &Baseline) -> serde_json::Value {
    if p.frames != u64::from(a.frames) || p.size != (u64::from(a.size.0), u64::from(a.size.1)) {
        eprintln!(
            "warning: the profile ran {} frames at {}×{}; this, {} at {}×{}",
            p.frames, p.size.0, p.size.1, a.frames, a.size.0, a.size.1
        );
    }
    let package = a.dir.file_name().map_or("".into(), |n| n.to_string_lossy().to_string());
    let points: Vec<serde_json::Value> = p
        .labels
        .iter()
        .filter_map(|(label, ms)| {
            let i = m.labels.iter().position(|l| l == label)?;
            let ran: Vec<Features> = m.frames.iter().filter_map(|f| f.get(&i).copied()).collect();
            (ran.len() * 2 > m.frames.len()).then(|| {
                serde_json::json!({
                    "name": format!("{package}: {label}"),
                    "m4_ms": ms,
                    "features": typical(&ran).json(),
                })
            })
        })
        .collect();
    serde_json::json!({
        "source": format!(
            "wrela profile --serial on the M4 and wrela estimate --pair on {}, {} frames at {}×{}",
            machine(), a.frames, a.size.0, a.size.1
        ),
        "points": points,
    })
}

/// This machine's CPU and lavapipe's threads, as the calibration names its own.
fn machine() -> String {
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "an unknown CPU".into());
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let threads = std::env::var("LP_NUM_THREADS").unwrap_or_else(|_| cores.to_string());
    format!("{cpu}, {cores} cores, LP_NUM_THREADS={threads}")
}

const CALIBRATION: &str = include_str!("estimate/calibration.json");

/// The recorded M4 times and their features on lavapipe, and the model they fit.
pub(crate) struct Calibration {
    machine: String,
    /// The features the model weighs (a subset of [`FEATURES`], by index).
    terms: Vec<usize>,
    points: Vec<Point>,
    /// Chrome's frame span over the serial mode's sum of passes, where both were recorded.
    chrome: Vec<(String, f64)>,
}

pub(crate) struct Point {
    name: String,
    m4_ms: f64,
    features: Features,
}

impl Calibration {
    fn parse(text: &str) -> Result<Calibration, String> {
        let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let terms = v["terms"]
            .as_array()
            .ok_or("no `terms`")?
            .iter()
            .map(|t| {
                let t = t.as_str().ok_or("a term isn't a string")?;
                FEATURES.iter().position(|f| *f == t).ok_or_else(|| format!("no feature `{t}`"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let points = v["points"]
            .as_array()
            .ok_or("no `points`")?
            .iter()
            .map(|p| {
                Ok(Point {
                    name: p["name"].as_str().ok_or("a point without a name")?.to_string(),
                    m4_ms: p["m4_ms"].as_f64().ok_or("a point without `m4_ms`")?,
                    features: Features::parse(&p["features"])?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let chrome = v["chrome"]
            .as_array()
            .map(|c| {
                c.iter()
                    .filter_map(|r| Some((r["name"].as_str()?.to_string(), r["ratio"].as_f64()?)))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Calibration {
            machine: v["machine"].as_str().unwrap_or("").to_string(),
            terms,
            points,
            chrome,
        })
    }
}

/// The model: each term's weight (ms per unit of its feature).
pub(crate) struct Model {
    terms: Vec<usize>,
    weights: Vec<f64>,
}

impl Model {
    /// The weights that fit `points` best, none negative: least squares on each point's error
    /// relative to its time (under 0.2 ms, relative to 0.2 ms, so a label too small to matter
    /// to a frame doesn't pull the fit).
    pub(crate) fn fit(terms: &[usize], points: &[&Point]) -> Model {
        let rows: Vec<(Vec<f64>, f64)> = points
            .iter()
            .map(|p| {
                let w = 1.0 / p.m4_ms.max(0.2);
                (terms.iter().map(|&t| p.features.get(t) * w).collect(), p.m4_ms * w)
            })
            .collect();
        Model { terms: terms.to_vec(), weights: nnls(&rows, terms.len()) }
    }

    pub(crate) fn ms(&self, f: &Features) -> f64 {
        self.terms.iter().zip(&self.weights).map(|(&t, w)| f.get(t) * w).sum()
    }
}

/// Non-negative least squares for a handful of unknowns: every subset of them, solved by the
/// normal equations, the best that's all non-negative. (2^n solves of n × n: n is a few.)
fn nnls(rows: &[(Vec<f64>, f64)], n: usize) -> Vec<f64> {
    let mut best = (f64::INFINITY, vec![0.0; n]);
    for mask in 0u32..1 << n {
        let cols: Vec<usize> = (0..n).filter(|i| mask & (1 << i) != 0).collect();
        let Some(x) = solve_normal(rows, &cols) else { continue };
        if x.iter().any(|&v| v < 0.0) {
            continue;
        }
        let mut full = vec![0.0; n];
        for (c, v) in cols.iter().zip(&x) {
            full[*c] = *v;
        }
        let err: f64 = rows
            .iter()
            .map(|(a, b)| (a.iter().zip(&full).map(|(x, w)| x * w).sum::<f64>() - b).powi(2))
            .sum();
        if err < best.0 {
            best = (err, full);
        }
    }
    best.1
}

/// The least squares solution over columns `cols`, by Gaussian elimination of the normal
/// equations with partial pivoting; None if they're singular.
fn solve_normal(rows: &[(Vec<f64>, f64)], cols: &[usize]) -> Option<Vec<f64>> {
    let k = cols.len();
    let mut m = vec![vec![0.0; k + 1]; k];
    for (a, b) in rows {
        for i in 0..k {
            for j in 0..k {
                m[i][j] += a[cols[i]] * a[cols[j]];
            }
            m[i][k] += a[cols[i]] * b;
        }
    }
    for c in 0..k {
        let p = (c..k).max_by(|&x, &y| m[x][c].abs().total_cmp(&m[y][c].abs()))?;
        if m[p][c].abs() < 1e-12 {
            return None;
        }
        m.swap(c, p);
        let pivot = m[c].clone();
        for (r, row) in m.iter_mut().enumerate() {
            if r != c {
                let f = row[c] / pivot[c];
                for (x, y) in row[c..].iter_mut().zip(&pivot[c..]) {
                    *x -= f * y;
                }
            }
        }
    }
    Some((0..k).map(|i| m[i][k] / m[i][i]).collect())
}

/// Each calibration point predicted by the model fitted to the others: (name, recorded ms,
/// predicted ms).
pub(crate) fn leave_one_out(c: &Calibration) -> Vec<(String, f64, f64)> {
    (0..c.points.len())
        .map(|i| {
            let others: Vec<&Point> =
                c.points.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, p)| p).collect();
            let model = Model::fit(&c.terms, &others);
            let p = &c.points[i];
            (p.name.clone(), p.m4_ms, model.ms(&p.features))
        })
        .collect()
}

/// The leave-one-out relative errors' sizes: their median, and the largest.
fn error(checks: &[(String, f64, f64)]) -> (f64, f64) {
    let mut errs: Vec<f64> = checks.iter().map(|(_, r, p)| ((p - r) / r).abs()).collect();
    errs.sort_by(f64::total_cmp);
    (errs.get(errs.len() / 2).copied().unwrap_or(0.0), errs.last().copied().unwrap_or(0.0))
}

/// A report: the model, its error, and each label's and the frame's estimate.
struct Report {
    model: Model,
    /// Leave-one-out relative errors: the median of their sizes, and the largest.
    error: (f64, f64),
    checks: Vec<(String, f64, f64)>,
    labels: Vec<LabelEstimate>,
    frame: (f64, f64),
    lvp_frame: f64,
    /// The smallest and largest ratio of Chrome's frame to the estimate, as recorded.
    chrome: Option<(f64, f64)>,
    machine_differs: bool,
    calibration_machine: String,
}

struct LabelEstimate {
    name: String,
    /// Estimated M4 ms: the typical frame's, and the 95th percentile of the frames it ran in.
    ms: f64,
    p95: f64,
    /// Lavapipe's ms, the typical frame's.
    lvp_ms: f64,
    /// The share of counted frames it ran in.
    share: f64,
}

impl Report {
    fn new(c: &Calibration, m: &Measured) -> Report {
        let all: Vec<&Point> = c.points.iter().collect();
        let model = Model::fit(&c.terms, &all);
        let checks = leave_one_out(c);
        let error = error(&checks);
        let n = m.frames.len().max(1) as f64;
        let mut labels: Vec<LabelEstimate> = (0..m.labels.len())
            .map(|i| {
                let ran: Vec<Features> =
                    m.frames.iter().filter_map(|f| f.get(&i).copied()).collect();
                let t = typical(&ran);
                let per: Vec<f64> = ran.iter().map(|f| model.ms(f)).collect();
                LabelEstimate {
                    name: m.labels[i].clone(),
                    ms: model.ms(&t),
                    p95: percentile(&per, 95).unwrap_or(0.0),
                    lvp_ms: t.lvp_ms,
                    share: ran.len() as f64 / n,
                }
            })
            .collect();
        labels.sort_by(|a, b| (b.ms * b.share).total_cmp(&(a.ms * a.share)));
        let totals: Vec<Features> = m
            .frames
            .iter()
            .map(|f| {
                let mut s = Features::default();
                for x in f.values() {
                    s.add(x);
                }
                s
            })
            .collect();
        let t = typical(&totals);
        let per: Vec<f64> = totals.iter().map(|f| model.ms(f)).collect();
        let ratios = c.chrome.iter().map(|(_, r)| *r);
        let chrome = (!c.chrome.is_empty())
            .then(|| ratios.fold((f64::INFINITY, 0.0f64), |(lo, hi), r| (lo.min(r), hi.max(r))));
        let here = machine();
        Report {
            frame: (model.ms(&t), percentile(&per, 95).unwrap_or(0.0)),
            lvp_frame: t.lvp_ms,
            model,
            error,
            checks,
            labels,
            chrome,
            machine_differs: !c.machine.is_empty() && c.machine != here,
            calibration_machine: c.machine.clone(),
        }
    }

    fn text(&self, a: &Args, m: &Measured) -> String {
        let mut s = format!(
            "`{}`: {} frames at {}×{} ({} counted), on {}\n\n",
            a.dir.display(),
            a.frames,
            a.size.0,
            a.size.1,
            m.frames.len(),
            m.adapter
        );
        if self.machine_differs {
            s.push_str(&format!(
                "warning: calibrated on {}; this is {}: lavapipe's times differ\n\n",
                self.calibration_machine,
                machine()
            ));
        }
        let (ms, p95) = self.frame;
        s.push_str(&format!(
            "MacBook Air M4, estimated: {ms:.2} ms a frame, each pass alone ({:.0} fps); 95th \
             percentile {p95:.2} ms\n",
            1000.0 / ms
        ));
        if let Some((lo, hi)) = self.chrome {
            s.push_str(&format!(
                "  in Chrome, back to back: {:.2} to {:.2} ms ({:.0} to {:.0} fps), by the ratios \
                 recorded ({lo:.2} to {hi:.2}: vertex work the M4 hides from a pass's time)\n",
                ms * lo,
                ms * hi,
                1000.0 / (ms * lo),
                1000.0 / (ms * hi)
            ));
        }
        s.push_str(&format!(
            "  the model's error, each recorded time predicted without it: median {:.0}%, worst \
             {:.0}% ({} points)\n\n",
            self.error.0 * 100.0,
            self.error.1 * 100.0,
            self.checks.len()
        ));
        s.push_str(&format!(
            "{:28} {:>9} {:>9} {:>12} {:>7}\n",
            "label", "M4 ms", "95th", "lavapipe ms", "frames"
        ));
        for l in &self.labels {
            s.push_str(&format!(
                "  {:26} {:9.3} {:9.3} {:12.1} {:6.0}%\n",
                l.name,
                l.ms,
                l.p95,
                l.lvp_ms,
                l.share * 100.0
            ));
        }
        s.push_str(&format!("\nlavapipe: {:.0} ms a frame\nthe model:", self.lvp_frame));
        for (t, w) in self.model.terms.iter().zip(&self.model.weights) {
            s.push_str(&format!(" {} × {w:.4}", FEATURES[*t]));
        }
        s.push('\n');
        s
    }

    fn json(&self, a: &Args, m: &Measured) -> serde_json::Value {
        serde_json::json!({
            "package": a.dir.display().to_string(),
            "size": [a.size.0, a.size.1],
            "frames": a.frames,
            "counted": m.frames.len(),
            "adapter": m.adapter,
            "machine": machine(),
            "m4": {
                "frame_ms": self.frame.0,
                "frame_p95_ms": self.frame.1,
                "fps": 1000.0 / self.frame.0,
                "chrome_frame_ms": self.chrome.map(|(lo, hi)| [self.frame.0 * lo, self.frame.0 * hi]),
                "labels": self.labels.iter().map(|l| serde_json::json!({
                    "label": l.name, "ms": l.ms, "p95_ms": l.p95, "lavapipe_ms": l.lvp_ms,
                    "share": l.share,
                })).collect::<Vec<_>>(),
            },
            "model": {
                "terms": self.model.terms.iter().map(|t| FEATURES[*t]).collect::<Vec<_>>(),
                "weights": self.model.weights,
                "error_median": self.error.0,
                "error_worst": self.error.1,
                "checks": self.checks.iter().map(|(n, r, p)| serde_json::json!({
                    "name": n, "m4_ms": r, "predicted_ms": p,
                })).collect::<Vec<_>>(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(name: &str, m4_ms: f64, items: f64, lvp_ms: f64, fragments: f64) -> Point {
        let features = Features { items, lvp_ms, fragments, ..Features::default() };
        Point { name: name.into(), m4_ms, features }
    }

    /// Points made by known weights give those weights back, and each is predicted exactly
    /// without itself.
    #[test]
    fn the_fit_finds_the_weights_that_made_the_points() {
        let terms = vec![0, 1, 4];
        let (a, b, c) = (0.02, 0.006, 0.3);
        let points: Vec<Point> =
            [(1.0, 100.0, 2.0), (6.0, 400.0, 0.5), (2.0, 50.0, 3.0), (3.0, 10.0, 1.0)]
                .iter()
                .enumerate()
                .map(|(i, &(n, l, f))| point(&i.to_string(), a * n + b * l + c * f, n, l, f))
                .collect();
        let model = Model::fit(&terms, &points.iter().collect::<Vec<_>>());
        for (got, want) in model.weights.iter().zip([a, b, c]) {
            assert!((got - want).abs() < 1e-9, "{got} for {want}");
        }
        let c = Calibration { machine: String::new(), terms, points, chrome: Vec::new() };
        for (name, recorded, predicted) in leave_one_out(&c) {
            assert!((recorded - predicted).abs() < 1e-9, "{name}: {predicted} for {recorded}");
        }
    }

    /// The shipped calibration parses and gives the model and the error that language.md and
    /// the report's readers were told: lavapipe's time alone, about 1/137 of it; leave-one-out
    /// error median 21%, worst 69%. A new calibration changes these on purpose, here and there.
    #[test]
    fn the_shipped_calibration_gives_the_model_it_claims() {
        let c = Calibration::parse(CALIBRATION).expect("calibration.json parses");
        assert!(c.points.len() >= 10, "{} points", c.points.len());
        let model = Model::fit(&c.terms, &c.points.iter().collect::<Vec<_>>());
        let lvp_ms = FEATURES.iter().position(|f| *f == "lvp_ms").expect("a feature");
        let lvp = c.terms.iter().position(|&t| t == lvp_ms).expect("lavapipe's time is a term");
        assert_eq!((1.0 / model.weights[lvp]).round(), 137.0, "{:?}", model.weights);
        let (median, worst) = error(&leave_one_out(&c));
        assert_eq!(((median * 100.0).round(), (worst * 100.0).round()), (21.0, 69.0));
    }

    /// A weight the points would make negative is held at zero, and the rest refit.
    #[test]
    fn no_weight_is_negative() {
        // Time falls as fragments rise: alone, fragments would take a negative weight.
        let points = [
            point("a", 1.0, 1.0, 100.0, 4.0),
            point("b", 2.0, 1.0, 200.0, 1.0),
            point("c", 3.0, 1.0, 300.0, 0.0),
        ];
        let model = Model::fit(&[1, 4], &points.iter().collect::<Vec<_>>());
        assert_eq!(model.weights[1], 0.0);
        assert!((model.weights[0] - 0.01).abs() < 1e-9, "{:?}", model.weights);
    }

    /// The typical frame is the mean of the middle half by lavapipe's time: a frame that
    /// compiled a shader (lavapipe compiles at first use) doesn't move it.
    #[test]
    fn a_slow_first_use_doesnt_move_the_typical_frame() {
        let mut frames: Vec<Features> = (0..8)
            .map(|i| Features { lvp_ms: 10.0 + i as f64, fragments: 1.0, ..Features::default() })
            .collect();
        frames[0].lvp_ms = 900.0;
        let t = typical(&frames);
        assert_eq!(t.lvp_ms, (13.0 + 14.0 + 15.0 + 16.0) / 4.0);
        assert_eq!(t.fragments, 1.0);
    }
}
