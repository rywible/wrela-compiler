//! Shared pieces of the end-to-end tests: building a package with the compiler, and running
//! hand-written WGSL (the spike's kernels) on the same GPU for comparison.
//!
//! The GPU tests are `#[ignore]`d: they need a GPU and take the GPU lock (`wrela_host::lock`).
//! `tools/check.sh` runs them with `--ignored`.

use std::path::{Path, PathBuf};

/// The repository's root.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Builds the package in `pkg` into `out`, as `wrela build` does. On errors, returns them
/// rendered.
pub fn build(pkg: &Path, out: &Path) -> Result<(), String> {
    let compiler = wrela_driver::Compiler::new(pkg);
    let (check, diags, files) = compiler.build();
    if diags.iter().any(|d| d.is_error()) {
        return Err(wrela_diag::render::render_all(&check.sources, &diags));
    }
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    for (path, bytes) in &files.files {
        std::fs::write(out.join(path), bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
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
/// `wrela_host::Host`, so don't make one while a host is loaded.
pub struct RawGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    // Declared last: dropped after the device.
    _lock: wrela_host::lock::GpuLock,
}

fn map_read(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Result<Vec<u8>, String> {
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    let bytes = slice.get_mapped_range().map_err(|e| e.to_string())?.to_vec();
    buffer.unmap();
    Ok(bytes)
}

impl RawGpu {
    pub fn new() -> Result<RawGpu, String> {
        let lock = wrela_host::lock::GpuLock::acquire("wrela-tests").map_err(|e| e.to_string())?;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .map_err(|e| format!("no GPU adapter: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("hand-written"),
            required_features: wgpu::Features::TIMESTAMP_QUERY,
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        }))
        .map_err(|e| format!("can't open the device: {e}"))?;
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
        device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| e.to_string())?;
        // Resolved in a later submission: on Metal, a resolve in the same command buffer as the
        // passes it times can read stale values (as in wrela-host).
        let size = u64::from(2 * reps) * 8;
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.resolve_query_set(&queries, 0..2 * reps, &resolve, 0);
        let mut readbacks = Vec::new();
        let ts = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        encoder.copy_buffer_to_buffer(&resolve, 0, &ts, 0, size);
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
        let ticks: Vec<u64> = map_read(device, &ts)?
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().expect("8 bytes")))
            .collect();
        let period = f64::from(queue.get_timestamp_period());
        let times = (0..reps as usize)
            .map(|r| (ticks[2 * r + 1].saturating_sub(ticks[2 * r])) as f64 * period)
            .collect();
        let mut outs = Vec::new();
        for rb in &readbacks {
            outs.push(map_read(device, rb)?);
        }
        Ok((times, outs))
    }
}
