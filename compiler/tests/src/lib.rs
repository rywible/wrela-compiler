//! Shared pieces of the end-to-end tests: building a package with the compiler, calling a
//! program's exports, reading case files, and running hand-written WGSL (the spike's kernels) on
//! the same GPU for comparison.
//!
//! The GPU tests are `#[ignore]`d: they need a GPU and take the GPU lock (`wrela_host::lock`).
//! `tools/check.sh` runs them with `--ignored`.
//!
//! Sampled tests have two sizes: small by default, so `cargo test` stays fast, and the size the
//! acceptance criteria name when `WRELA_FULL` is set ([`sized`]), as `tools/check.sh` does.

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

/// Applies every edit of the first fix of each diagnostic to `text` (all in one file). Edits
/// that overlap an earlier one are skipped. Used by tests that check a fix removes its
/// diagnostic.
pub fn apply_fixes(text: &str, file: FileId, diags: &[Diagnostic]) -> String {
    let mut edits: Vec<&Edit> = diags
        .iter()
        .filter_map(|d| d.fixes.first())
        .flat_map(|f| f.edits.iter())
        .filter(|e| e.span.file == file)
        .collect();
    edits.sort_by_key(|e| (e.span.start, e.span.end));
    let mut out = String::with_capacity(text.len());
    let mut pos = 0usize;
    for e in edits {
        let (s, t) = (e.span.start as usize, e.span.end as usize);
        if s < pos {
            continue;
        }
        out.push_str(&text[pos..s]);
        out.push_str(&e.replacement);
        pos = t;
    }
    out.push_str(&text[pos..]);
    out
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
