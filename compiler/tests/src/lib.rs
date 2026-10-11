//! Shared pieces of the end-to-end tests: building a package with the compiler, calling a
//! program's exports, reading case files, and running hand-written WGSL (the spike's kernels) on
//! the same GPU for comparison.
//!
//! The GPU tests are `#[ignore]`d: they need a GPU and take the GPU lock (`wrela_host::lock`).
//! `tools/check.sh` runs them (four at a time share the GPU), but not those whose reason starts
//! `long:` (headless Chrome, the clearing's camera path, soak runs), which `tools/check.sh
//! --long` runs, nor `measure:` (time budgets and comparisons), which `tools/check.sh --full`
//! runs one at a time, the GPU theirs alone.
//!
//! Sampled tests have two sizes: small by default, so `cargo test` and the long checks stay
//! fast, and the size the acceptance criteria name when `WRELA_FULL` is set ([`sized`]), as
//! `tools/check.sh --full` does.

pub mod camera;
pub mod spike01;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use wrela_diag::{Diagnostic, Edit, FileId};
use wrela_host::{CpuHost, GpuTiming, Host, TickLog, Timing, Value, map_read};

/// SplitMix64, for tests' random inputs.
pub use wrela_grammar::rng::Rng;
pub use wrela_grammar::testing::{files_under, par_each, par_map, repo_root, sized};

/// Work done once per process for each key: a second caller with a key waits for the first's
/// work rather than racing it, and work for other keys runs at the same time.
pub struct OncePerKey(Mutex<BTreeMap<String, Arc<OnceLock<()>>>>);

impl OncePerKey {
    pub const fn new() -> OncePerKey {
        OncePerKey(Mutex::new(BTreeMap::new()))
    }

    /// Does `work` for `key` unless it's done (or another thread is doing it: this waits).
    pub fn run(&self, key: &str, work: impl FnOnce()) {
        let once =
            self.0.lock().unwrap_or_else(|p| p.into_inner()).entry(key.into()).or_default().clone();
        once.get_or_init(work);
    }
}

impl Default for OncePerKey {
    fn default() -> OncePerKey {
        OncePerKey::new()
    }
}

/// Builds the package in `pkg` as `wrela build --testing` does (with its `@testing` exports),
/// without writing its files: the output, or the errors rendered.
pub fn build(pkg: &Path) -> Result<wrela_driver::Output, String> {
    let built = wrela_driver::build_for_tests(pkg, false);
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

/// Copies the package at `from` to `to` ([`copy_dir`]), each relative `path = "…"` in its
/// wrela.toml made absolute against `from`: its dependencies are found from the copy too.
pub fn copy_package(from: &Path, to: &Path) {
    copy_dir(from, to);
    let manifest = to.join("wrela.toml");
    let text = std::fs::read_to_string(&manifest).expect("wrela.toml");
    const KEY: &str = "path = \"";
    let (mut out, mut rest) = (String::new(), text.as_str());
    while let Some(at) = rest.find(KEY) {
        let start = at + KEY.len();
        let end = start + rest[start..].find('"').expect("a path's closing quote");
        let path = Path::new(&rest[start..end]);
        let path = if path.is_absolute() { path.to_path_buf() } else { from.join(path) };
        out += &format!("{}path = {:?}", &rest[..at], path.display().to_string());
        rest = &rest[end + 1..];
    }
    out += rest;
    std::fs::write(&manifest, out).expect("write wrela.toml");
}

/// Writes `edit` of the file `file` of the package at `pkg`, which must change it: when it was
/// written.
pub fn write_edit(pkg: &Path, file: &str, edit: impl Fn(&str) -> String) -> std::time::Instant {
    let path = pkg.join(file);
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{file}: {e}"));
    let new = edit(&src);
    assert_ne!(new, src, "the edit changes {file}");
    std::fs::write(&path, new).unwrap_or_else(|e| panic!("write {file}: {e}"));
    std::time::Instant::now()
}

/// Waits until the `wrela run` server has had `n` changes shown: the `n`th. Panics after
/// `limit`.
/// Waits until a page is running on the `wrela run` server: it has asked for changes. Panics
/// after `limit`. A test edits after this, so its page sees each edit as a change, not as part of
/// the build it loaded (Chrome opens after the machine's Chrome lock, and a build is quicker).
pub fn wait_polling(server: &wrela_driver::live::Server, limit: std::time::Duration) {
    let began = std::time::Instant::now();
    while server.polls() == 0 {
        assert!(began.elapsed() < limit, "no page asked for changes in {}s", limit.as_secs());
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

pub fn wait_shown(
    server: &wrela_driver::live::Server,
    n: usize,
    limit: std::time::Duration,
) -> wrela_driver::live::Shown {
    let began = std::time::Instant::now();
    loop {
        let shown = server.shown();
        if shown.len() >= n {
            return shown[n - 1].clone();
        }
        assert!(
            began.elapsed() < limit,
            "change {n} wasn't shown in {limit:?}: {:?}",
            server.changes()
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
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

/// Calls the export `name`, which must return one `vec3` (three `f32`s).
pub fn one_vec3(host: &mut impl Exports, name: &str, args: &[Value]) -> [f32; 3] {
    match host.call(name, args).expect(name).as_slice() {
        [Value::F32(x), Value::F32(y), Value::F32(z)] => [*x, *y, *z],
        other => panic!("{name} returned {other:?}"),
    }
}

/// Calls the export `name`, which must return one `vec4` (four `f32`s).
pub fn one_vec4(host: &mut impl Exports, name: &str, args: &[Value]) -> [f32; 4] {
    match host.call(name, args).expect(name).as_slice() {
        [Value::F32(x), Value::F32(y), Value::F32(z), Value::F32(w)] => [*x, *y, *z, *w],
        other => panic!("{name} returned {other:?}"),
    }
}

/// The mean of |a − b| over the colour channels of some pixels (`pixels`, their indices) of two
/// RGBA8 images, in /255; the share of those pixels with a channel more than 8/255 apart; and
/// how many pixels there were.
pub fn image_difference_over(
    a: &[u8],
    b: &[u8],
    pixels: impl IntoIterator<Item = usize>,
) -> (f64, f64, usize) {
    let (mut sum, mut over, mut n) = (0.0, 0usize, 0usize);
    for p in pixels {
        let mut most = 0u8;
        for c in 0..3 {
            let d = a[4 * p + c].abs_diff(b[4 * p + c]);
            sum += f64::from(d);
            most = most.max(d);
        }
        over += usize::from(most > 8);
        n += 1;
    }
    let n_f = n.max(1) as f64;
    (sum / (3.0 * n_f), over as f64 / n_f, n)
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

/// The rest positions of the first `n` of a buffer's skinned vertices (`SkinVertex`, as the
/// spike wrote them too): 48 bytes each, the position first.
pub fn positions(bytes: &[u8], n: usize) -> Vec<[f32; 3]> {
    bytes.chunks_exact(48).take(n).map(|v| f32s(&v[..12]).try_into().expect("3 floats")).collect()
}

/// The larger error; NaN if either is (`f64::max` drops a NaN, which would pass every check).
pub fn worse(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) }
}

/// The median of some durations.
pub fn median(xs: &[f64]) -> f64 {
    percentile(xs, 0.5)
}

/// The `p` quantile of some durations (0.99: the 99th percentile): the ⌊n·p⌋th of the n
/// sorted (from 0), or the last.
pub fn percentile(xs: &[f64], p: f64) -> f64 {
    let mut v = xs.to_vec();
    v.sort_by(f64::total_cmp);
    v[((v.len() as f64 * p) as usize).min(v.len() - 1)]
}

/// The manifest of the build in `dir`.
pub fn manifest(dir: &Path) -> wrela_abi::Manifest {
    let text = std::fs::read_to_string(dir.join("manifest.json")).expect("manifest.json");
    wrela_abi::Manifest::parse(&text).expect("a valid manifest")
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

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// The first `size` bytes of `src` (a `COPY_SRC` buffer), after the work submitted so far.
    pub fn read(&self, src: &wgpu::Buffer, size: u64) -> Vec<u8> {
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(src, 0, &readback, 0, size);
        self.queue.submit([encoder.finish()]);
        map_read(&self.device, &readback).expect("read back")
    }

    /// Runs `entry` of `wgsl` over `groups` workgroups (in x), `reps` times, each in its own
    /// timed compute pass. Returns each run's GPU nanoseconds and the `Write` buffers'
    /// contents after the last. Panics if the WGSL isn't valid or the GPU fails.
    pub fn run(
        &self,
        wgsl: &str,
        entry: &str,
        binds: &[Bind],
        groups: u32,
        reps: u32,
    ) -> (Vec<f64>, Vec<Vec<u8>>) {
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
            panic!("`{entry}`: {e}");
        }
        let times = wrela_host::read_timestamps(device, queue, &queries, reps)
            .unwrap_or_else(|e| panic!("`{entry}`'s timestamps: {e}"));
        let outs = binds
            .iter()
            .zip(&buffers)
            .filter_map(|(b, buf)| match b {
                Bind::Write(n) => Some(self.read(buf, *n)),
                _ => None,
            })
            .collect();
        (times, outs)
    }
}

/// Builds the package at `pkg` (a path from the repo root) into `target/tmp/<name>`, where
/// tools/headless.py can serve it as a page (its runs write `results/` there). Returns the
/// directory and its path from the repo root. Each package is built once per process, and each
/// page is a copy of that build.
pub fn page(pkg: &str, name: &str) -> (PathBuf, String) {
    page_built_by(pkg, name, "release", |p| wrela_driver::build_for_tests(p, false))
}

/// [`page`] with the build a player loads: a release build, not a test build (no `@testing`
/// exports, and none of the pipelines only tests reach).
pub fn shipped_page(pkg: &str, name: &str) -> (PathBuf, String) {
    page_built_by(pkg, name, "shipped", wrela_driver::build)
}

/// [`page`] with a debug build (language.md §11's checks).
pub fn debug_page(pkg: &str, name: &str) -> (PathBuf, String) {
    page_built_by(pkg, name, "debug", |p| wrela_driver::build_for_tests(p, true))
}

/// [`page`] with a build whose WASM uses no SIMD (language.md §11).
pub fn scalar_page(pkg: &str, name: &str) -> (PathBuf, String) {
    page_built_by(pkg, name, "scalar", wrela_driver::build_without_simd)
}

/// The build of the package at `pkg` that [`page`] copies, made once per process: for a test
/// that only reads its files (its manifest, its WGSL). Nothing may load it or write beside it.
pub fn page_build(pkg: &str) -> PathBuf {
    built_once(pkg, "release", |p| wrela_driver::build_for_tests(p, false))
}

/// [`page`], the package built by `build` (`kind` names it).
fn page_built_by(
    pkg: &str,
    name: &str,
    kind: &str,
    build: fn(&Path) -> wrela_driver::Output,
) -> (PathBuf, String) {
    let base = built_once(pkg, kind, build);
    let rel = format!("target/tmp/{name}");
    let dir = repo_root().join(&rel);
    let _ = std::fs::remove_dir_all(&dir);
    copy_dir(&base, &dir);
    (dir, rel)
}

/// The package at `pkg` built by `build` (`kind` names it), once per process: its directory.
fn built_once(pkg: &str, kind: &str, build: fn(&Path) -> wrela_driver::Output) -> PathBuf {
    static BUILDS: OncePerKey = OncePerKey::new();
    // No host loads the build here (it would keep its compiled code beside it), only copies.
    let base = repo_root()
        .join("target/tmp/page-builds")
        .join(format!("{}-{kind}", pkg.replace('/', "-")));
    BUILDS.run(&base.display().to_string(), || {
        let _ = std::fs::remove_dir_all(&base);
        let built = build(&repo_root().join(pkg));
        if built.has_errors() {
            panic!(
                "{pkg} doesn't build:\n{}",
                wrela_diag::render::render_all(&built.sources, &built.diagnostics)
            );
        }
        built.write_to(&base).expect("write the build");
    });
    base
}

/// Runs tools/headless.py, from the repo root, with `args`: whether the run passed. It empties
/// the page's `results/` first.
fn headless<S: AsRef<std::ffi::OsStr>>(args: impl IntoIterator<Item = S>) -> bool {
    headless_with(args, None)
}

/// [`headless`], the test server slowed to `throttle` bits a second if given.
fn headless_with<S: AsRef<std::ffi::OsStr>>(
    args: impl IntoIterator<Item = S>,
    throttle: Option<String>,
) -> bool {
    let mut cmd = std::process::Command::new("python3");
    cmd.arg(repo_root().join("tools/headless.py")).args(args).current_dir(repo_root());
    match throttle {
        Some(bits) => cmd.env("WRELA_THROTTLE", bits),
        None => cmd.env_remove("WRELA_THROTTLE"),
    };
    cmd.status().expect("python3 runs tools/headless.py").success()
}

/// Runs a plain page (no wrela build) at `rel` in headless Chrome with URL fragment
/// `fragment`: whether it wrote `ok` to `results/DONE` within `timeout` seconds.
pub fn run_page_in_chrome(rel: &str, fragment: &str, timeout: u32) -> bool {
    headless([rel, fragment, &timeout.to_string()])
}

/// A test-mode run in headless Chrome that must fail: the message the page failed with.
pub fn chrome_failure(rel: &str, run: ChromeRun) -> String {
    assert!(!run.run(rel), "the browser run passed; it should have failed");
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
    /// Each pass's GPU time, when [`ChromeRun::timing`] asked for them, as the native host
    /// records them.
    pub timings: Vec<GpuTiming>,
    /// Each frame's span (ms), when [`ChromeRun::timing`] asked for them: from its first timed
    /// pass's start to its last's end ([`wrela_host::frame_spans`]).
    pub spans: Vec<(usize, f64)>,
    /// The ticker's ticks, when the program started one.
    pub ticks: Option<ChromeTicks>,
    /// When each frame began (ms since 1970), and the frame each printed line came in
    /// (`results/frames.json`), with the lines (`results/log.txt`).
    pub began_ms: Vec<f64>,
    pub printed: Vec<(usize, String)>,
}

/// What the ticker's thread did in a test-mode run in Chrome (`results/ticks.json`).
pub struct ChromeTicks {
    pub hz: u32,
    /// Each tick's CPU time (ms).
    pub cpu_ms: Vec<f64>,
    /// The tick log (`results/ticks.log`), with each tick's state hash, unless the run kept no
    /// hashes.
    pub log: Option<TickLog>,
}

/// The script of input events a [`ChromeRun`] writes beside the page.
const SCRIPT: &str = "input.json";

/// How [`run_in_chrome_with`] runs a page: test mode's parameters (runtime/browser's
/// testmode.ts).
#[derive(Clone, Debug, Default)]
pub struct ChromeRun {
    pub frames: u32,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Threads for parallel jobs, the program's own included.
    pub workers: u32,
    /// Quanta of the program's voice to render offline (0: none).
    pub audio: u32,
    /// Time each pass on the GPU (`timestamps=1`), or each pass and dispatch run alone, one at a
    /// time, so each time is its own (`timestamps=2`).
    pub timing: Timing,
    /// A script of input events (runtime/abi `input`), which the run writes beside the page.
    pub script: Option<String>,
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
    /// Makes every shader unique, so its pipelines are created cold, and times creating them
    /// (`results/pipelines.json`; 0: none).
    pub salt: u32,
    /// Each frame as soon as the last is done, not at its time.
    pub saturate: bool,
    /// The network slowed to this many bits a second (the test server's `WRELA_THROTTLE`: every
    /// request shares the rate); 0 for full speed.
    pub throttle: u64,
}

impl ChromeRun {
    pub fn new(frames: u32, width: u32, height: u32, fps: f64) -> ChromeRun {
        ChromeRun { frames, width, height, fps, workers: 1, ..ChromeRun::default() }
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
            timing,
            script,
            latency,
            keylatency,
            paced,
            tickdelay,
            framedelay,
            nohash,
            salt,
            saturate,
            // The server's, not the page's.
            throttle: _,
        } = self;
        let mut fragment = format!(
            "#test&frames={frames}&width={width}&height={height}&fps={fps}&workers={workers}"
        );
        // Test mode takes no zeros: a parameter that's off is left out.
        let flag = |on: &bool| u32::from(*on);
        let timestamps = match timing {
            Timing::Off => 0,
            Timing::Span => 1,
            Timing::Serial => 2,
        };
        for (name, n) in [
            ("audio", *audio),
            ("timestamps", timestamps),
            ("latency", *latency),
            ("keylatency", *keylatency),
            ("paced", flag(paced)),
            ("tickdelay", *tickdelay),
            ("framedelay", *framedelay),
            ("nohash", flag(nohash)),
            ("salt", *salt),
            ("saturate", flag(saturate)),
        ] {
            if n > 0 {
                fragment += &format!("&{name}={n}");
            }
        }
        if script.is_some() {
            fragment += &format!("&input={SCRIPT}");
        }
        fragment
    }

    /// Runs the page at `rel` in headless Chrome this way, its script written beside it first:
    /// whether the run passed.
    fn run(&self, rel: &str) -> bool {
        if let Some(script) = &self.script {
            std::fs::write(repo_root().join(rel).join(SCRIPT), script).expect("write the script");
        }
        headless_with(
            [rel, &self.fragment(), &self.timeout().to_string()],
            (self.throttle > 0).then(|| self.throttle.to_string()),
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
    assert!(run.run(rel), "the browser run failed; see {rel}/results/console.log");
    let results = repo_root().join(rel).join("results");
    let frames = result_json(&results, "frames.json");
    let log = std::fs::read_to_string(results.join("log.txt")).expect("log.txt");
    let timings = if run.timing == Timing::Off { Vec::new() } else { timings(&results) };
    BrowserRun {
        hash: std::fs::read_to_string(results.join("hash.txt")).expect("hash.txt").trim().into(),
        frame: std::fs::read(results.join("frame.rgba")).expect("frame.rgba"),
        worker_chunks: std::fs::read_to_string(results.join("workers.txt"))
            .expect("workers.txt")
            .trim()
            .parse()
            .expect("a count"),
        audio: if run.audio > 0 {
            f32s(&std::fs::read(results.join("audio.f32")).expect("audio.f32"))
        } else {
            Vec::new()
        },
        spans: wrela_host::frame_spans(&timings),
        timings,
        ticks: chrome_ticks(&results),
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
    let log = std::fs::read(results.join("ticks.log")).ok();
    Some(ChromeTicks {
        hz: v["hz"].as_u64().expect("hz") as u32,
        cpu_ms: v["cpu_ms"].as_array().expect("cpu_ms").iter().map(number).collect(),
        log: log.map(|bytes| TickLog::decode(&bytes).expect("Chrome's tick log reads")),
    })
}

/// Each pass's GPU time in a test-mode run's `results/` (its `timings.json`), as the native
/// host records them.
pub fn timings(results: &Path) -> Vec<GpuTiming> {
    let v = result_json(results, "timings.json");
    let entries = v.as_array().expect("an array").iter();
    entries
        .map(|t| GpuTiming {
            frame: t["frame"].as_u64().expect("a frame") as usize,
            label: t["label"].as_str().expect("a label").to_string(),
            nanos: number(&t["nanos"]),
            start: number(&t["start"]),
            end: number(&t["end"]),
            submission: t["submission"].as_u64().expect("a submission") as usize,
        })
        .collect()
}

/// Runs the page another server serves at `url` (the studio server) in headless Chrome
/// (tools/headless.py --url): its results go to `page/results/`. Panics, pointing at the
/// console log, if the run fails.
pub fn run_url_in_chrome(url: &str, page: &Path, timeout: u32) {
    let timeout = timeout.to_string();
    let args: [&std::ffi::OsStr; 4] =
        ["--url".as_ref(), url.as_ref(), page.as_os_str(), timeout.as_ref()];
    assert!(
        headless(args),
        "the browser run failed; see {}",
        page.join("results/console.log").display()
    );
}
