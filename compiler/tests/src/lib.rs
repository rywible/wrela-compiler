//! Shared pieces of the end-to-end tests: building a package with the compiler, calling a
//! program's exports, reading case files, and running hand-written WGSL (the spike's kernels) on
//! the same GPU for comparison.
//!
//! The GPU tests are `#[ignore]`d: they need a GPU and take the GPU lock (`wrela_host::lock`).
//! `tools/check.sh` runs them with `--ignored`.
//!
//! Sampled tests have two sizes: small by default, so `cargo test` stays fast, and the size the
//! acceptance criteria name when `WRELA_FULL` is set ([`sized`]), as `tools/check.sh` does.

pub mod spike01;

use std::path::{Path, PathBuf};
use wrela_diag::{Diagnostic, Edit, FileId};
use wrela_host::{CpuHost, Host, Value, map_read};

/// SplitMix64, for tests' random inputs.
pub use wrela_grammar::rng::Rng;
pub use wrela_grammar::testing::{files_under, par_each, repo_root, sized};

/// Builds the package in `pkg` as `wrela build` does, without writing its files: the output,
/// or the errors rendered.
pub fn build(pkg: &Path) -> Result<wrela_driver::Output, String> {
    let built = wrela_driver::build(pkg);
    if built.has_errors() {
        return Err(wrela_diag::render::render_all(&built.sources, &built.diagnostics));
    }
    Ok(built)
}

/// Builds the package in `pkg` into `out`, as `wrela build` does. Panics with the errors if it
/// doesn't build.
pub fn must_build(pkg: &Path, out: &Path) {
    let built = build(pkg).unwrap_or_else(|e| panic!("{} doesn't build:\n{e}", pkg.display()));
    built.write_to(out).expect("write the build");
}

/// Copies the files under `from` (as [`files_under`] finds them) to the same paths under `to`.
pub fn copy_dir(from: &Path, to: &Path) {
    for file in files_under(from, &[]) {
        let dest = to.join(file.strip_prefix(from).expect("a file under `from`"));
        std::fs::create_dir_all(dest.parent().expect("a parent")).expect("make a directory");
        std::fs::copy(&file, &dest).expect("copy a file");
    }
}

/// The cases of a case-file suite: the `.wrela` files and the directories in `dir`, sorted.
pub fn cases(dir: &Path) -> Vec<PathBuf> {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("a directory entry").path())
        .filter(|p| p.is_dir() || p.extension().is_some_and(|x| x == "wrela"))
        .collect();
    cases.sort();
    cases
}

/// The values of a case file's `// <key>: <value>` lines, trimmed. Only the comment block at
/// the start of `text` is read.
pub fn header<'a>(text: &'a str, key: &str) -> impl Iterator<Item = &'a str> {
    let prefix = format!("// {key}:");
    text.lines()
        .take_while(|l| l.starts_with("//"))
        .filter_map(move |l| l.strip_prefix(prefix.as_str()).map(str::trim))
}

/// Applies every edit of the first fix of each diagnostic to `text` (all in one file). An edit
/// the same as the one before it (two diagnostics suggesting it) is made once; edits that
/// overlap otherwise can't both be made, so that's a test failure. Used by tests that check a
/// fix removes its diagnostic.
pub fn apply_fixes(text: &str, file: FileId, diags: &[Diagnostic]) -> String {
    let mut edits: Vec<&Edit> = diags
        .iter()
        .filter_map(|d| d.fixes.first())
        .flat_map(|f| f.edits.iter())
        .filter(|e| e.span.file == file)
        .collect();
    edits.sort_by_key(|e| (e.span.start, e.span.end));
    edits.dedup_by(|a, b| a.span == b.span && a.replacement == b.replacement);
    wrela_diag::apply_edits(text, edits)
}

/// A loaded program whose exports a test calls: a [`CpuHost`] or a [`Host`].
pub trait Exports {
    fn call(&mut self, name: &str, args: &[Value]) -> wrela_host::Result<Vec<Value>>;
}

impl Exports for CpuHost {
    fn call(&mut self, name: &str, args: &[Value]) -> wrela_host::Result<Vec<Value>> {
        self.call_export(name, args)
    }
}

impl Exports for Host {
    fn call(&mut self, name: &str, args: &[Value]) -> wrela_host::Result<Vec<Value>> {
        self.call_export(name, args)
    }
}

/// Calls the export `name`, which must return one `f32`.
pub fn one_f32(host: &mut impl Exports, name: &str, args: &[Value]) -> f32 {
    match host.call(name, args).expect(name).as_slice() {
        [Value::F32(x)] => *x,
        other => panic!("{name} returned {other:?}"),
    }
}

/// Calls the export `name`, which must return one `u32` (an `i32` in WASM).
pub fn one_u32(host: &mut impl Exports, name: &str, args: &[Value]) -> u32 {
    match host.call(name, args).expect(name).as_slice() {
        [Value::I32(x)] => *x as u32,
        other => panic!("{name} returned {other:?}"),
    }
}

/// A little-endian `f32` array from bytes.
pub fn f32s(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

pub fn u32s(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

pub fn bytes_of(xs: &[f32]) -> Vec<u8> {
    xs.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// The larger error; NaN if either is (`f64::max` drops a NaN, which would pass every check).
pub fn worse(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) }
}

/// The median of some durations.
pub fn median(xs: &[f64]) -> f64 {
    let mut v = xs.to_vec();
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// One binding of a hand-written kernel, at group 0 and the binding's index in the list.
pub enum Bind<'a> {
    Uniform(&'a [u8]),
    Read(&'a [u8]),
    /// A read-write storage buffer of this many bytes, read back after the runs.
    Write(u64),
}

/// A GPU device for running WGSL directly, with timestamps. It holds the GPU lock, like a
/// `wrela_host::Host` (one thread can hold both; other threads wait).
pub struct RawGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    // Declared last: dropped after the device.
    _lock: wrela_host::lock::GpuLock,
}

impl RawGpu {
    pub fn new() -> Result<RawGpu, String> {
        let lock = wrela_host::lock::GpuLock::acquire("wrela-tests").map_err(|e| e.to_string())?;
        let (device, queue) =
            wrela_host::open_device("hand-written", wgpu::Features::TIMESTAMP_QUERY)
                .map_err(|e| e.to_string())?;
        Ok(RawGpu { device, queue, _lock: lock })
    }

    /// Runs `entry` of `wgsl` over `groups` workgroups (in x), `reps` times, each in its own
    /// timed compute pass. Returns each run's GPU nanoseconds and the `Write` buffers'
    /// contents after the last.
    pub fn run(
        &self,
        wgsl: &str,
        entry: &str,
        binds: &[Bind],
        groups: u32,
        reps: u32,
    ) -> Result<(Vec<f64>, Vec<Vec<u8>>), String> {
        use wgpu::util::DeviceExt;
        let (device, queue) = (&self.device, &self.queue);
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(entry),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: None,
            module: &module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            cache: None,
        });
        let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC;
        let buffers: Vec<wgpu::Buffer> = binds
            .iter()
            .map(|b| match b {
                Bind::Uniform(data) => {
                    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: None,
                        contents: data,
                        usage: wgpu::BufferUsages::UNIFORM,
                    })
                }
                Bind::Read(data) => device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: data,
                    usage: storage,
                }),
                Bind::Write(size) => device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: *size,
                    usage: storage,
                    mapped_at_creation: false,
                }),
            })
            .collect();
        let entries: Vec<wgpu::BindGroupEntry> = buffers
            .iter()
            .enumerate()
            .map(|(i, b)| wgpu::BindGroupEntry {
                binding: i as u32,
                resource: b.as_entire_binding(),
            })
            .collect();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &entries,
        });
        let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: None,
            ty: wgpu::QueryType::Timestamp,
            count: 2 * reps,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        for r in 0..reps {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                    query_set: &queries,
                    beginning_of_pass_write_index: Some(2 * r),
                    end_of_pass_write_index: Some(2 * r + 1),
                }),
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(groups, 1, 1);
        }
        queue.submit([encoder.finish()]);
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(e.to_string());
        }
        let times = wrela_host::read_timestamps(device, queue, &queries, reps)
            .map_err(|e| e.to_string())?;
        let mut encoder = device.create_command_encoder(&Default::default());
        let mut readbacks = Vec::new();
        for (b, buf) in binds.iter().zip(&buffers) {
            if let Bind::Write(n) = b {
                let rb = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: *n,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                encoder.copy_buffer_to_buffer(buf, 0, &rb, 0, *n);
                readbacks.push(rb);
            }
        }
        queue.submit([encoder.finish()]);
        let mut outs = Vec::new();
        for rb in &readbacks {
            outs.push(map_read(device, rb).map_err(|e| e.to_string())?);
        }
        Ok((times, outs))
    }
}

/// Builds the package at `pkg` (a path from the repo root) into `target/tmp/<name>`, where
/// tools/headless.py can serve it as a page, with an empty `results/`. Returns the directory
/// and its path from the repo root.
pub fn page(pkg: &str, name: &str) -> (PathBuf, String) {
    let rel = format!("target/tmp/{name}");
    let dir = repo_root().join(&rel);
    let _ = std::fs::remove_dir_all(&dir);
    must_build(&repo_root().join(pkg), &dir);
    std::fs::create_dir_all(dir.join("results")).expect("results dir");
    (dir, rel)
}

/// [`page`] with a debug build (language.md §11's checks).
pub fn debug_page(pkg: &str, name: &str) -> (PathBuf, String) {
    page_built_by(pkg, name, wrela_driver::build_debug)
}

/// [`page`] with a build whose WASM uses no SIMD (language.md §11).
pub fn scalar_page(pkg: &str, name: &str) -> (PathBuf, String) {
    page_built_by(pkg, name, wrela_driver::build_without_simd)
}

fn page_built_by(
    pkg: &str,
    name: &str,
    build: fn(&Path) -> wrela_driver::Output,
) -> (PathBuf, String) {
    let rel = format!("target/tmp/{name}");
    let dir = repo_root().join(&rel);
    let _ = std::fs::remove_dir_all(&dir);
    let built = build(&repo_root().join(pkg));
    if built.has_errors() {
        panic!(
            "{pkg} doesn't build:\n{}",
            wrela_diag::render::render_all(&built.sources, &built.diagnostics)
        );
    }
    built.write_to(&dir).expect("write the build");
    std::fs::create_dir_all(dir.join("results")).expect("results dir");
    (dir, rel)
}

/// Runs tools/headless.py, from the repo root, with `args`: whether the run passed.
fn headless<S: AsRef<std::ffi::OsStr>>(args: impl IntoIterator<Item = S>) -> bool {
    std::process::Command::new("python3")
        .arg(repo_root().join("tools/headless.py"))
        .args(args)
        .current_dir(repo_root())
        .status()
        .expect("python3 runs tools/headless.py")
        .success()
}

/// A test-mode run in headless Chrome that must fail: the message the page failed with.
pub fn chrome_failure(rel: &str, run: ChromeRun) -> String {
    assert!(
        !headless([rel, &run.fragment(), &run.timeout().to_string()]),
        "the browser run passed; it should have failed"
    );
    let done = repo_root().join(rel).join("results/DONE");
    std::fs::read_to_string(done).expect("results/DONE")
}

/// What a page's test-mode run in headless Chrome gave: its state hash, and its last frame
/// (RGBA8, rows top to bottom).
pub struct BrowserRun {
    pub hash: String,
    pub frame: Vec<u8>,
    /// How many chunks of parallel jobs the workers ran.
    pub worker_chunks: u32,
    /// The voice's samples, when [`ChromeRun::audio`] asked for them.
    pub audio: Vec<f32>,
    /// Each pass's GPU time, when [`ChromeRun::timestamps`] asked for them: its frame, its
    /// label (as the native host's `GpuTiming`) and nanoseconds.
    pub timings: Vec<(usize, String, f64)>,
    /// The ticker's ticks, when the program started one.
    pub ticks: Option<ChromeTicks>,
    /// Each frame's CPU time (ms), when it began (ms since 1970), and the frame each printed
    /// line came in (`results/frames.json`), with the lines (`results/log.txt`).
    pub cpu_ms: Vec<f64>,
    pub began_ms: Vec<f64>,
    pub printed: Vec<(usize, String)>,
}

/// What the ticker's thread did in a test-mode run in Chrome (`results/ticks.json`).
pub struct ChromeTicks {
    pub hz: u32,
    /// When each tick began (ms since 1970), and its CPU time (ms).
    pub began_ms: Vec<f64>,
    pub cpu_ms: Vec<f64>,
    /// Each tick's state hash (16 hex digits), unless the run kept none.
    pub hashes: Vec<String>,
    /// Ticks the clock dropped, catching up.
    pub dropped: u64,
    /// The tick log (runtime/abi `ticks`), unless the run kept no hashes.
    pub log: Option<Vec<u8>>,
}

/// How [`run_in_chrome_with`] runs a page: test mode's parameters (runtime/browser's
/// testmode.ts).
#[derive(Clone, Debug)]
pub struct ChromeRun {
    pub frames: u32,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Threads for parallel jobs, the program's own included.
    pub workers: u32,
    /// Quanta of the program's voice to render offline (0: none).
    pub audio: u32,
    /// Time each pass on the GPU.
    pub timestamps: bool,
    /// A script of input events, relative to the page (runtime/abi `input`); "" for none.
    pub input: String,
    /// Pointer events the page sends through the DOM while the frames run (0: none):
    /// `results/latency.json` says when each reached the program.
    pub latency: u32,
    /// Right-arrow key presses the page sends through the DOM while the frames run (0: none).
    pub keylatency: u32,
    /// Ticks on the ticker's own clock, not in lockstep with the frames (#43 §2.3).
    pub paced: bool,
    /// Ms each tick, and each frame, is held longer.
    pub tickdelay: u32,
    pub framedelay: u32,
    /// Keep no state hashes (they cost CPU time a timing run shouldn't count).
    pub nohash: bool,
}

impl ChromeRun {
    pub fn new(frames: u32, width: u32, height: u32, fps: f64) -> ChromeRun {
        ChromeRun {
            frames,
            width,
            height,
            fps,
            workers: 1,
            audio: 0,
            timestamps: false,
            input: String::new(),
            latency: 0,
            keylatency: 0,
            paced: false,
            tickdelay: 0,
            framedelay: 0,
            nohash: false,
        }
    }

    /// Seconds the run may take: 300, or three times its frames' paced time and two minutes
    /// more, for a long run.
    fn timeout(&self) -> u32 {
        (f64::from(self.frames) / self.fps * 3.0 + 120.0).max(300.0) as u32
    }

    /// The page's URL fragment that asks for this run.
    fn fragment(&self) -> String {
        let ChromeRun {
            frames,
            width,
            height,
            fps,
            workers,
            audio,
            timestamps,
            input,
            latency,
            keylatency,
            paced,
            tickdelay,
            framedelay,
            nohash,
        } = self;
        let count = |name: &str, n: u32| if n > 0 { format!("&{name}={n}") } else { String::new() };
        format!(
            "#test&frames={frames}&width={width}&height={height}&fps={fps}&workers={workers}{}{}{}{}{}{}{}{}{}",
            count("audio", *audio),
            if *timestamps { "&timestamps=1" } else { "" },
            if input.is_empty() { String::new() } else { format!("&input={input}") },
            count("latency", *latency),
            count("keylatency", *keylatency),
            if *paced { "&paced=1" } else { "" },
            count("tickdelay", *tickdelay),
            count("framedelay", *framedelay),
            if *nohash { "&nohash=1" } else { "" },
        )
    }
}

/// Runs the page at `rel` (from [`page`]) in headless Chrome's test mode for `frames` frames at
/// `fps`, `width` × `height`. Panics, pointing at the console log, if the run fails.
pub fn run_in_chrome(rel: &str, frames: u32, width: u32, height: u32, fps: f64) -> BrowserRun {
    run_in_chrome_with(rel, ChromeRun::new(frames, width, height, fps))
}

/// [`run_in_chrome`], with all of test mode's parameters.
pub fn run_in_chrome_with(rel: &str, run: ChromeRun) -> BrowserRun {
    let results = repo_root().join(rel).join("results");
    // A run before this one, on the same page, left its own results.
    let _ = std::fs::remove_dir_all(&results);
    std::fs::create_dir_all(&results).expect("results dir");
    let passed = headless([rel, &run.fragment(), &run.timeout().to_string()]);
    assert!(passed, "the browser run failed; see {rel}/results/console.log");
    let frames = result_json(&results, "frames.json");
    let log = std::fs::read_to_string(results.join("log.txt")).expect("log.txt");
    BrowserRun {
        hash: std::fs::read_to_string(results.join("hash.txt")).expect("hash.txt").trim().into(),
        frame: std::fs::read(results.join("frame.rgba")).expect("frame.rgba"),
        worker_chunks: std::fs::read_to_string(results.join("workers.txt"))
            .expect("workers.txt")
            .trim()
            .parse()
            .expect("a count"),
        audio: if run.audio > 0 {
            let bytes = std::fs::read(results.join("audio.f32")).expect("audio.f32");
            bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
        } else {
            Vec::new()
        },
        timings: if run.timestamps { timings(&results) } else { Vec::new() },
        ticks: chrome_ticks(&results),
        cpu_ms: frames["cpu_ms"].as_array().expect("cpu_ms").iter().map(number).collect(),
        began_ms: frames["began_ms"].as_array().expect("began_ms").iter().map(number).collect(),
        printed: frames["printed_in"]
            .as_array()
            .expect("printed_in")
            .iter()
            .map(|f| f.as_u64().expect("a frame") as usize)
            .zip(log.lines().map(str::to_string))
            .collect(),
    }
}

fn number(v: &serde_json::Value) -> f64 {
    v.as_f64().expect("a number")
}

/// A test-mode run's `results/<name>`, as JSON.
pub fn result_json(results: &Path, name: &str) -> serde_json::Value {
    let text =
        std::fs::read_to_string(results.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name} isn't JSON: {e}"))
}

/// The ticker's ticks in a test-mode run's `results/`, if the program started one.
fn chrome_ticks(results: &Path) -> Option<ChromeTicks> {
    if !results.join("ticks.json").exists() {
        return None;
    }
    let v = result_json(results, "ticks.json");
    let numbers = |key: &str| v[key].as_array().expect(key).iter().map(number).collect();
    Some(ChromeTicks {
        hz: v["hz"].as_u64().expect("hz") as u32,
        began_ms: numbers("began_ms"),
        cpu_ms: numbers("cpu_ms"),
        hashes: v["hashes"]
            .as_array()
            .expect("hashes")
            .iter()
            .map(|h| h.as_str().expect("a hash").to_string())
            .collect(),
        dropped: v["dropped"].as_u64().expect("dropped"),
        log: std::fs::read(results.join("ticks.log")).ok(),
    })
}

/// Each pass's GPU time in a test-mode run's `results/` (its `timings.json`): its frame, its
/// label (as the native host's `GpuTiming`) and nanoseconds.
pub fn timings(results: &Path) -> Vec<(usize, String, f64)> {
    let text = std::fs::read_to_string(results.join("timings.json")).expect("timings.json");
    let v: serde_json::Value = serde_json::from_str(&text).expect("timings.json is JSON");
    let entries = v.as_array().expect("an array").iter();
    entries
        .map(|t| {
            let frame = t["frame"].as_u64().expect("a frame") as usize;
            let label = t["label"].as_str().expect("a label").to_string();
            (frame, label, t["nanos"].as_f64().expect("nanoseconds"))
        })
        .collect()
}

/// Runs the page another server serves at `url` (the studio server) in headless Chrome
/// (tools/headless.py --url): its results go to `page/results/`. Panics, pointing at the
/// console log, if the run fails.
pub fn run_url_in_chrome(url: &str, page: &Path, timeout: u32) {
    let results = page.join("results");
    let _ = std::fs::remove_dir_all(&results);
    std::fs::create_dir_all(&results).expect("results dir");
    let timeout = timeout.to_string();
    let args: [&std::ffi::OsStr; 4] =
        ["--url".as_ref(), url.as_ref(), page.as_os_str(), timeout.as_ref()];
    assert!(
        headless(args),
        "the browser run failed; see {}",
        results.join("console.log").display()
    );
}
