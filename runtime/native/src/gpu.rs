//! The GPU side: carries out decoded commands with wgpu, headless.
//!
//! The "screen" is an offscreen [`wrela_abi::manifest::SCREEN_FORMAT`] texture that can be read
//! back. How commands map to wgpu (the browser runtime's decoder does the same with WebGPU):
//!
//! - Work is recorded into one pending command encoder and submitted ("flushed") at `Present`,
//!   before a `WriteBuffer` (so earlier dispatches see the old contents), and at the end of each
//!   frame. Each dispatch gets its own compute pass.
//! - A screen pass's draws are collected and recorded into one render pass at `Present`, since a
//!   pass can span batches.
//! - Uniform bytes go into a ring buffer bound with dynamic offsets (aligned to the device's
//!   offset alignment, 256 by default). The ring is staged on the CPU and written just before
//!   each flush, so every command in a submission has its own slice. It grows (after a flush)
//!   when one submission needs more than it holds.
//! - Bind group 0 of each pipeline is created from the manifest: the uniform block (dynamic
//!   offset) and the buffers, in order. Bind groups are cached per (pipeline, buffer handles).
//!
//! Commands arrive decoded, sequenced and checked ([`crate::check`]); a wgpu validation error
//! here is a host bug, reported as [`Error::Gpu`].

use crate::error::{Error, Result};
use crate::program::Executor;
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use wgpu::util::align_to;
use wrela_abi::manifest::{Access, BufferBinding, Manifest, Stage, UniformSpace};
use wrela_abi::stream::{Command, Opcode};

/// The screen's format: `SCREEN_FORMAT` (rgba8unorm) in wgpu's spelling.
pub const SCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// The uniform ring's starting size.
const RING_START: u64 = 64 * 1024;
/// Timestamp queries per submission (two per timed pass).
const TIMESTAMP_QUERIES: u32 = 512;

/// One timed piece of GPU work, from timestamp queries.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuTiming {
    /// Index of the frame in the run.
    pub frame: usize,
    /// The dispatch's pipeline name, or `screen pass`.
    pub label: String,
    pub nanos: f64,
}

struct Pipeline {
    name: String,
    kind: Kind,
    layout: wgpu::BindGroupLayout,
    /// The uniform block's binding and size, if it has one.
    uniform: Option<(u32, u32)>,
    buffers: Vec<BufferBinding>,
}

enum Kind {
    Compute(wgpu::ComputePipeline),
    Render(wgpu::RenderPipeline),
}

struct Buffer {
    buffer: wgpu::Buffer,
    size: u32,
}

struct PendingDraw {
    pipeline: u32,
    vertices: u32,
    instances: u32,
    buffers: Vec<u32>,
    uniforms: Vec<u8>,
}

/// A screen pass between `BeginScreenPass` and `Present`.
struct ScreenPass {
    clear: [f32; 4],
    draws: Vec<PendingDraw>,
}

struct Screen {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

struct UniformRing {
    buffer: wgpu::Buffer,
    capacity: u64,
    /// This submission's uniform bytes, written to `buffer` at the flush.
    staging: Vec<u8>,
}

struct Timer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    /// What each pair of queries in this submission times: (frame, label).
    pending: Vec<(usize, String)>,
    period: f32,
    results: Vec<GpuTiming>,
}

pub(crate) struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    limits: wgpu::Limits,
    align: u64,
    pipelines: Vec<Pipeline>,
    buffers: HashMap<u32, Buffer>,
    bind_groups: HashMap<(u32, Vec<u32>), wgpu::BindGroup>,
    ring: UniformRing,
    encoder: Option<wgpu::CommandEncoder>,
    pass: Option<ScreenPass>,
    screen: Option<Screen>,
    timer: Option<Timer>,
    errors: Arc<Mutex<Vec<String>>>,
    frame: usize,
}

fn gpu_err(e: impl std::fmt::Display) -> Error {
    Error::Gpu(e.to_string())
}

/// Opens the high-performance adapter's device with `features` and WebGPU's default limits, as
/// a browser's `requestDevice()` gives: both hosts accept the same programs.
pub fn open_device(label: &str, features: wgpu::Features) -> Result<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .map_err(|e| Error::Gpu(format!("no GPU adapter: {e}")))?;
    let missing = features - adapter.features();
    if !missing.is_empty() {
        return Err(Error::Gpu(format!("{} doesn't support {missing:?}", adapter.get_info().name)));
    }
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some(label),
        required_features: features,
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .map_err(|e| Error::Gpu(format!("can't open the device: {e}")))
}

impl Gpu {
    /// Opens the GPU and builds every pipeline. `shaders[i]` is pipeline `i`'s WGSL source.
    pub(crate) fn new(manifest: &Manifest, shaders: &[String], timestamps: bool) -> Result<Gpu> {
        let features =
            if timestamps { wgpu::Features::TIMESTAMP_QUERY } else { wgpu::Features::empty() };
        let (device, queue) = open_device("wrela-host", features)?;

        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = errors.clone();
        device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
            sink.lock().unwrap_or_else(|p| p.into_inner()).push(e.to_string());
        }));
        let sink = errors.clone();
        device.set_device_lost_callback(move |reason, message| {
            sink.lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(format!("device lost ({reason:?}): {message}"));
        });

        let limits = device.limits();
        let align = u64::from(
            limits
                .min_uniform_buffer_offset_alignment
                .max(limits.min_storage_buffer_offset_alignment),
        );
        let ring = UniformRing {
            buffer: create_ring(&device, RING_START),
            capacity: RING_START,
            staging: Vec::new(),
        };
        let timer = timestamps.then(|| Timer::new(&device, &queue));
        let mut gpu = Gpu {
            device,
            queue,
            limits,
            align,
            pipelines: Vec::new(),
            buffers: HashMap::new(),
            bind_groups: HashMap::new(),
            ring,
            encoder: None,
            pass: None,
            screen: None,
            timer,
            errors,
            frame: 0,
        };
        for (p, source) in manifest.pipelines.iter().zip(shaders) {
            let pipeline = gpu.build_pipeline(p, source)?;
            gpu.pipelines.push(pipeline);
        }
        gpu.check_errors()?;
        Ok(gpu)
    }

    fn build_pipeline(&self, p: &wrela_abi::manifest::Pipeline, source: &str) -> Result<Pipeline> {
        let shader_err = |message: String| Error::Shader {
            pipeline: p.name.clone(),
            shader: p.shader.clone(),
            message,
        };
        let is_compute = matches!(p.stage, Stage::Compute { .. });
        let all = if is_compute {
            wgpu::ShaderStages::COMPUTE
        } else {
            wgpu::ShaderStages::VERTEX_FRAGMENT
        };
        let mut entries = Vec::new();
        if let Some(u) = &p.uniform {
            let limit = match u.space {
                UniformSpace::Uniform => self.limits.max_uniform_buffer_binding_size,
                UniformSpace::Storage => self.limits.max_storage_buffer_binding_size,
            };
            if u64::from(u.size) > limit {
                return Err(shader_err(format!(
                    "its uniform block is {} bytes; the limit is {limit}",
                    u.size
                )));
            }
            let ty = match u.space {
                UniformSpace::Uniform => wgpu::BufferBindingType::Uniform,
                UniformSpace::Storage => wgpu::BufferBindingType::Storage { read_only: true },
            };
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: u.binding,
                visibility: all,
                ty: wgpu::BindingType::Buffer {
                    ty,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(u64::from(u.size)),
                },
                count: None,
            });
        }
        for b in &p.buffers {
            let read_only = b.access == Access::Read;
            // WebGPU forbids writable storage in vertex shaders.
            let visibility =
                if read_only || is_compute { all } else { wgpu::ShaderStages::FRAGMENT };
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: b.binding,
                visibility,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            });
        }

        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let layout = self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&p.name),
            entries: &entries,
        });
        let pipeline_layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&p.name),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&p.shader),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let kind = match &p.stage {
            Stage::Compute { entry, .. } => Kind::Compute(self.device.create_compute_pipeline(
                &wgpu::ComputePipelineDescriptor {
                    label: Some(&p.name),
                    layout: Some(&pipeline_layout),
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    cache: None,
                },
            )),
            Stage::Render { vertex_entry, fragment_entry } => {
                Kind::Render(self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(&p.name),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some(vertex_entry),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    // Triangle list, counter-clockwise front faces, no culling: WebGPU's defaults.
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some(fragment_entry),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: SCREEN_FORMAT,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                }))
            }
        };
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(shader_err(e.to_string()));
        }
        Ok(Pipeline {
            name: p.name.clone(),
            kind,
            layout,
            uniform: p.uniform.as_ref().map(|u| (u.binding, u.size)),
            buffers: p.buffers.clone(),
        })
    }

    /// Makes the screen `width` x `height` (a new, cleared texture if the size changed).
    pub(crate) fn set_screen(&mut self, width: u32, height: u32) -> Result<()> {
        let max = self.limits.max_texture_dimension_2d;
        if width == 0 || height == 0 || width > max || height > max {
            return Err(Error::Gpu(format!(
                "a {width}x{height} screen is outside 1..={max} in each dimension"
            )));
        }
        if self.screen.as_ref().is_some_and(|s| (s.width, s.height) == (width, height)) {
            return Ok(());
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("screen"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.screen = Some(Screen { texture, view, width, height });
        Ok(())
    }

    pub(crate) fn set_frame(&mut self, frame: usize) {
        self.frame = frame;
    }

    pub(crate) fn take_timings(&mut self) -> Vec<GpuTiming> {
        self.timer.as_mut().map(|t| std::mem::take(&mut t.results)).unwrap_or_default()
    }

    /// Reports (and clears) errors the device raised outside an error scope.
    fn check_errors(&self) -> Result<()> {
        let mut errors = self.errors.lock().unwrap_or_else(|p| p.into_inner());
        if errors.is_empty() {
            return Ok(());
        }
        let all = errors.join("\n");
        errors.clear();
        Err(Error::Gpu(all))
    }

    fn bind_group(&mut self, pipeline: u32, handles: &[u32]) -> wgpu::BindGroup {
        let key = (pipeline, handles.to_vec());
        if let Some(bg) = self.bind_groups.get(&key) {
            return bg.clone();
        }
        let p = &self.pipelines[pipeline as usize];
        let mut entries = Vec::new();
        if let Some((binding, size)) = p.uniform {
            entries.push(wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &self.ring.buffer,
                    offset: 0,
                    size: NonZeroU64::new(u64::from(size)),
                }),
            });
        }
        for (handle, b) in handles.iter().zip(&p.buffers) {
            entries.push(wgpu::BindGroupEntry {
                binding: b.binding,
                resource: self.buffers[handle].buffer.as_entire_binding(),
            });
        }
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&p.name),
            layout: &p.layout,
            entries: &entries,
        });
        self.bind_groups.insert(key, bg.clone());
        bg
    }

    /// Makes room for `bytes` more uniform bytes (counted with alignment) in this submission,
    /// flushing first or growing the ring if it must. Call before recording the work that uses
    /// them, since a flush submits what's recorded.
    fn reserve_uniforms(&mut self, opcode: Opcode, bytes: u64) -> Result<()> {
        let start = align_to(self.ring.staging.len() as u64, self.align);
        if start + bytes <= self.ring.capacity {
            return Ok(());
        }
        self.flush()?;
        if bytes > self.ring.capacity {
            let capacity = bytes.max(self.ring.capacity * 2).next_power_of_two();
            if capacity > self.limits.max_buffer_size {
                return Err(Error::command(
                    opcode,
                    format!("{bytes} uniform bytes in one submission is over the limit"),
                ));
            }
            self.ring = UniformRing {
                buffer: create_ring(&self.device, capacity),
                capacity,
                staging: Vec::new(),
            };
            // Cached bind groups point at the old ring.
            self.bind_groups.clear();
        }
        Ok(())
    }

    /// Appends uniform bytes to this submission's slice of the ring; returns their offset.
    /// [`Gpu::reserve_uniforms`] must have made room.
    fn push_uniforms(&mut self, bytes: &[u8]) -> Option<u32> {
        if bytes.is_empty() {
            return None;
        }
        let offset = align_to(self.ring.staging.len() as u64, self.align);
        debug_assert!(offset + bytes.len() as u64 <= self.ring.capacity);
        self.ring.staging.resize(offset as usize, 0);
        self.ring.staging.extend_from_slice(bytes);
        Some(offset as u32)
    }

    fn aligned(&self, len: usize) -> u64 {
        align_to(len as u64, self.align)
    }

    fn encoder(&mut self) -> &mut wgpu::CommandEncoder {
        self.encoder.get_or_insert_with(|| {
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default())
        })
    }

    /// Submits everything recorded so far, after writing its uniform bytes.
    pub(crate) fn flush(&mut self) -> Result<()> {
        let Some(encoder) = self.encoder.take() else {
            debug_assert!(self.ring.staging.is_empty());
            return Ok(());
        };
        if !self.ring.staging.is_empty() {
            self.queue.write_buffer(&self.ring.buffer, 0, &self.ring.staging);
            self.ring.staging.clear();
        }
        self.queue.submit([encoder.finish()]);
        if let Some(timer) = &mut self.timer
            && !timer.pending.is_empty()
        {
            timer.collect(&self.device, &self.queue)?;
        }
        Ok(())
    }

    /// Makes room for one timed pass's queries in this submission, flushing first if they're
    /// used up. Call before recording the pass (a flush submits what's recorded).
    fn reserve_timestamps(&mut self) -> Result<()> {
        if self.timer.as_ref().is_some_and(|t| t.pending.len() as u32 * 2 >= TIMESTAMP_QUERIES) {
            self.flush()?;
        }
        Ok(())
    }

    /// The first of two timestamp query indices for the pass about to be recorded, when timing.
    /// [`Gpu::reserve_timestamps`] must have made room.
    fn next_timestamps(&mut self, label: &str) -> Option<u32> {
        let frame = self.frame;
        self.timer.as_mut().map(|t| {
            t.pending.push((frame, label.to_string()));
            (t.pending.len() as u32 - 1) * 2
        })
    }

    fn create_buffer(&mut self, handle: u32, size: u32) {
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("buffer {handle}")),
            size: u64::from(size),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        self.buffers.insert(handle, Buffer { buffer, size });
    }

    fn write_buffer(&mut self, handle: u32, offset: u32, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        // Dispatches recorded before this write must see the old contents.
        self.flush()?;
        self.queue.write_buffer(&self.buffers[&handle].buffer, u64::from(offset), data);
        Ok(())
    }

    fn dispatch(
        &mut self,
        pipeline: u32,
        groups: [u32; 3],
        buffers: &[u32],
        uniforms: &[u8],
    ) -> Result<()> {
        let op = Opcode::Dispatch;
        let name = self.pipelines[pipeline as usize].name.clone();
        // Everything that can flush happens before anything is recorded for this dispatch.
        self.reserve_uniforms(op, self.aligned(uniforms.len()))?;
        self.reserve_timestamps()?;
        let timestamps = self.next_timestamps(&name);
        let offsets: Vec<u32> = self.push_uniforms(uniforms).into_iter().collect();
        let bind_group = self.bind_group(pipeline, buffers);
        let query_set = self.timer.as_ref().map(|t| t.queries.clone());
        let Kind::Compute(compute) = &self.pipelines[pipeline as usize].kind else {
            unreachable!("the checker checked the kind");
        };
        let compute = compute.clone();
        let encoder = self.encoder();
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(&name),
            timestamp_writes: timestamps.zip(query_set.as_ref()).map(|(i, query_set)| {
                wgpu::ComputePassTimestampWrites {
                    query_set,
                    beginning_of_pass_write_index: Some(i),
                    end_of_pass_write_index: Some(i + 1),
                }
            }),
        });
        pass.set_pipeline(&compute);
        pass.set_bind_group(0, &bind_group, &offsets);
        pass.dispatch_workgroups(groups[0], groups[1], groups[2]);
        Ok(())
    }

    fn draw(
        &mut self,
        pipeline: u32,
        vertices: u32,
        instances: u32,
        buffers: &[u32],
        uniforms: &[u8],
    ) {
        let draw = PendingDraw {
            pipeline,
            vertices,
            instances,
            buffers: buffers.to_vec(),
            uniforms: uniforms.to_vec(),
        };
        self.pass
            .get_or_insert_with(|| ScreenPass { clear: [0.0; 4], draws: Vec::new() })
            .draws
            .push(draw);
    }

    fn present(&mut self) -> Result<()> {
        let op = Opcode::Present;
        let Some(pass) = self.pass.take() else {
            unreachable!("the sequencer checked a screen pass is open");
        };
        if self.screen.is_none() {
            return Err(Error::command(
                op,
                "there's no screen: the host draws only while running frames",
            ));
        }
        // Everything that can flush happens before anything is recorded for this pass.
        let total = pass.draws.iter().map(|d| self.aligned(d.uniforms.len())).sum();
        self.reserve_uniforms(op, total)?;
        self.reserve_timestamps()?;
        let timestamps = self.next_timestamps("screen pass");
        let mut recorded = Vec::with_capacity(pass.draws.len());
        for d in &pass.draws {
            let offsets: Vec<u32> = self.push_uniforms(&d.uniforms).into_iter().collect();
            let bind_group = self.bind_group(d.pipeline, &d.buffers);
            let Kind::Render(render) = &self.pipelines[d.pipeline as usize].kind else {
                unreachable!("the checker checked the kind");
            };
            recorded.push((render.clone(), bind_group, offsets, d.vertices, d.instances));
        }
        let query_set = self.timer.as_ref().map(|t| t.queries.clone());
        let screen = self.screen.as_ref().map(|s| s.view.clone()).expect("checked above");
        let [r, g, b, a] = pass.clear.map(f64::from);
        let encoder = self.encoder();
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("screen pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &screen,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: timestamps.zip(query_set.as_ref()).map(|(i, query_set)| {
                    wgpu::RenderPassTimestampWrites {
                        query_set,
                        beginning_of_pass_write_index: Some(i),
                        end_of_pass_write_index: Some(i + 1),
                    }
                }),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for (pipeline, bind_group, offsets, vertices, instances) in &recorded {
                rp.set_pipeline(pipeline);
                rp.set_bind_group(0, bind_group, offsets);
                rp.draw(0..*vertices, 0..*instances);
            }
        }
        self.flush()
    }

    /// Copies a buffer back to the CPU (waits for the GPU).
    pub(crate) fn read_buffer(&mut self, handle: u32) -> Result<Vec<u8>> {
        self.flush()?;
        let size = self
            .buffers
            .get(&handle)
            .map(|b| u64::from(b.size))
            .ok_or_else(|| Error::Gpu(format!("there's no buffer {handle}")))?;
        let staging = self.readback_buffer(size);
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(&self.buffers[&handle].buffer, 0, &staging, 0, size);
        self.queue.submit([encoder.finish()]);
        let bytes = map_read(&self.device, &staging)?;
        self.check_errors()?;
        Ok(bytes)
    }

    /// Copies the screen back: RGBA8, rows top to bottom, no padding (waits for the GPU).
    pub(crate) fn read_screen(&mut self) -> Result<Vec<u8>> {
        self.flush()?;
        let screen =
            self.screen.as_ref().ok_or_else(|| Error::Gpu("there's no screen yet".into()))?;
        let (width, height) = (screen.width, screen.height);
        let row = u64::from(width) * 4;
        let padded = align_to(row, u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT));
        let staging = self.readback_buffer(padded * u64::from(height));
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            screen.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded as u32),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        let bytes = map_read(&self.device, &staging)?;
        self.check_errors()?;
        Ok(bytes.chunks_exact(padded as usize).flat_map(|r| &r[..row as usize]).copied().collect())
    }

    fn readback_buffer(&self, size: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }
}

impl Executor for Gpu {
    fn execute(&mut self, cmd: &Command<'_>) -> Result<()> {
        match cmd {
            Command::CreateBuffer { handle, size } => {
                self.create_buffer(*handle, *size);
                Ok(())
            }
            Command::WriteBuffer { handle, offset, data } => {
                self.write_buffer(*handle, *offset, data)
            }
            Command::Dispatch { pipeline, groups, buffers, uniforms } => {
                self.dispatch(*pipeline, *groups, buffers, uniforms)
            }
            Command::BeginScreenPass { clear } => {
                self.pass = Some(ScreenPass { clear: *clear, draws: Vec::new() });
                Ok(())
            }
            Command::Draw { pipeline, vertices, instances, buffers, uniforms } => {
                self.draw(*pipeline, *vertices, *instances, buffers, uniforms);
                Ok(())
            }
            Command::Present => self.present(),
        }
    }

    fn end_frame(&mut self) -> Result<()> {
        self.flush()?;
        self.check_errors()
    }
}

fn create_ring(device: &wgpu::Device, capacity: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("uniform ring"),
        size: capacity,
        usage: wgpu::BufferUsages::UNIFORM
            | wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Maps a `MAP_READ` buffer and copies it out, waiting for the GPU.
pub fn map_read(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Result<Vec<u8>> {
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).map_err(gpu_err)?;
    rx.recv().map_err(gpu_err)?.map_err(gpu_err)?;
    let bytes = slice.get_mapped_range().map_err(gpu_err)?.to_vec();
    buffer.unmap();
    Ok(bytes)
}

impl Timer {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Timer {
        let size = u64::from(TIMESTAMP_QUERIES) * 8;
        Timer {
            queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: TIMESTAMP_QUERIES,
            }),
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("timestamps"),
                size,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            readback: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("timestamps readback"),
                size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            pending: Vec::new(),
            period: queue.get_timestamp_period(),
            results: Vec::new(),
        }
    }

    /// Reads back the timestamps of the submission just made. This waits for the GPU at every
    /// flush, so the timing mode measures GPU durations, not throughput.
    ///
    /// The queries are resolved in a later submission, after the timed one completes: on
    /// Apple GPUs (wgpu's Metal backend) a resolve in the same command buffer as the passes it
    /// times can read the previous submission's values.
    fn collect(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<()> {
        device.poll(wgpu::PollType::wait_indefinitely()).map_err(gpu_err)?;
        let n = self.pending.len() as u32 * 2;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.resolve_query_set(&self.queries, 0..n, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.readback, 0, u64::from(n) * 8);
        queue.submit([encoder.finish()]);
        let bytes = map_read(device, &self.readback)?;
        let ticks: Vec<u64> = bytes
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().expect("8 bytes")))
            .collect();
        for (i, (frame, label)) in self.pending.drain(..).enumerate() {
            let elapsed = ticks[2 * i + 1].saturating_sub(ticks[2 * i]);
            self.results.push(GpuTiming {
                frame,
                label,
                nanos: elapsed as f64 * f64::from(self.period),
            });
        }
        Ok(())
    }
}
