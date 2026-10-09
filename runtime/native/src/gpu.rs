//! The GPU side: carries out decoded commands with wgpu, headless.
//!
//! The "screen" is an offscreen [`wrela_abi::manifest::SCREEN_FORMAT`] texture that can be read
//! back. How commands map to wgpu (the browser runtime's decoder does the same with WebGPU):
//!
//! - Work is recorded into one pending command encoder and submitted ("flushed") at the end of
//!   a pass on the screen, before a `WriteBuffer` or `WriteTexture` (so earlier work sees the old
//!   contents), before a readback, and at the end of each call. Each dispatch gets its own
//!   compute pass.
//! - A pass's draws are collected and recorded into one render pass when it ends (`Present` or
//!   `EndPass`), since a pass can span batches. A pass that may join the one before (`join`), and
//!   begins right after it ended on the same targets, keeping what's in them, runs as part of
//!   it, unless the timing is serial: on a tile-based GPU each pass stores its targets to memory
//!   and the next loads them back. Only a pass the program marks: a pass's geometry is binned
//!   before its tiles are drawn, so two heavy passes made one wait longer than they save.
//! - A render pipeline is made for each combination of target formats it's drawn with: the
//!   screen's or a texture's colour format, and a depth format or none. With depth, a fragment
//!   is kept where its depth is less than what's there, which it replaces.
//! - Uniform bytes go into a ring buffer bound with dynamic offsets (aligned to the device's
//!   offset alignment, 256 by default). The ring is staged on the CPU and written just before
//!   each flush, so every command in a submission has its own slice. It grows (after a flush)
//!   when one submission needs more than it holds.
//! - Bind group 0 of each pipeline is made from the manifest: the uniform block (dynamic offset)
//!   and the bindings, in order. Bind groups are cached per (pipeline, bindings), until one of
//!   their resources is destroyed.
//! - Destroying a resource drops this host's reference to it; wgpu keeps it alive for work
//!   already recorded.
//!
//! Commands arrive decoded, sequenced and checked ([`wrela_abi::check`]); a wgpu validation error
//! here is a host bug, reported as [`Error::Gpu`].

use crate::Timing;
use crate::error::{Error, Result};
use crate::program::Executor;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use wgpu::util::align_to;
use wrela_abi::manifest::{
    BindingKind, BindingStage, Cull, DepthBias, DepthState, Manifest, ResourceBinding, Stage,
    UniformSpace,
};
use wrela_abi::stream::{self, Binding, Command, Compare, Opcode, Pass, TextureFormat};

/// The screen's format: `SCREEN_FORMAT` (rgba8unorm) in wgpu's spelling.
const SCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// The uniform ring's starting size.
const RING_START: u64 = 64 * 1024;
/// Timestamp queries per submission (two per timed pass).
const TIMESTAMP_QUERIES: u32 = 512;

/// One timed piece of GPU work, from timestamp queries.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuTiming {
    /// Index of the frame in the run.
    pub frame: usize,
    /// The name the program gave it (`std::gpu::label`), else the dispatch's pipeline name, or
    /// `screen pass` or `pass`.
    pub label: String,
    /// Its duration, `end - start`.
    pub nanos: f64,
    /// When it started and ended on the GPU's clock, in nanoseconds from an origin of its own:
    /// a frame's span is its first start to its last end.
    pub start: f64,
    pub end: f64,
}

/// Each frame's span in `timings` (ms): from its first timed piece's start to its last's end,
/// by frame, for the frames that have any.
pub fn frame_spans(timings: &[GpuTiming]) -> Vec<(usize, f64)> {
    let mut spans: std::collections::BTreeMap<usize, (f64, f64)> = Default::default();
    for t in timings {
        let e = spans.entry(t.frame).or_insert((t.start, t.end));
        e.0 = e.0.min(t.start);
        e.1 = e.1.max(t.end);
    }
    spans.into_iter().map(|(f, (a, b))| (f, (b - a) / 1e6)).collect()
}

/// A texture format in wgpu's spelling.
fn wgpu_format(f: TextureFormat) -> wgpu::TextureFormat {
    match f {
        TextureFormat::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
        TextureFormat::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
        TextureFormat::Depth32Float => wgpu::TextureFormat::Depth32Float,
        TextureFormat::R16Float => wgpu::TextureFormat::R16Float,
        TextureFormat::Rg16Float => wgpu::TextureFormat::Rg16Float,
        TextureFormat::R32Float => wgpu::TextureFormat::R32Float,
        TextureFormat::R32Uint => wgpu::TextureFormat::R32Uint,
    }
}

/// A colour texture binding's format, which the manifest's parser requires of it.
fn colour_format(b: &ResourceBinding) -> TextureFormat {
    b.format.expect("the manifest's parser gives every colour texture binding its format")
}

/// A texture binding's view: 3D or 2D.
fn view_dimension(kind: BindingKind) -> wgpu::TextureViewDimension {
    if kind.is_3d() { wgpu::TextureViewDimension::D3 } else { wgpu::TextureViewDimension::D2 }
}

/// How a shader reads a colour texture of format `f`: filtered floats, unfiltered floats, or
/// unsigned integers.
fn sample_type(f: TextureFormat) -> wgpu::TextureSampleType {
    if f.is_uint() {
        wgpu::TextureSampleType::Uint
    } else {
        wgpu::TextureSampleType::Float { filterable: f.filterable() }
    }
}

fn wgpu_compare(c: Compare) -> wgpu::CompareFunction {
    match c {
        Compare::Less => wgpu::CompareFunction::Less,
        Compare::LessEqual => wgpu::CompareFunction::LessEqual,
        Compare::Greater => wgpu::CompareFunction::Greater,
        Compare::GreaterEqual => wgpu::CompareFunction::GreaterEqual,
        Compare::Equal => wgpu::CompareFunction::Equal,
        Compare::NotEqual => wgpu::CompareFunction::NotEqual,
        Compare::Always => wgpu::CompareFunction::Always,
        Compare::Never => wgpu::CompareFunction::Never,
    }
}

/// A live resource the program made.
enum Resource {
    Buffer(wgpu::Buffer),
    Texture { view: wgpu::TextureView, texture: wgpu::Texture, format: TextureFormat },
    Sampler(wgpu::Sampler),
}

/// The target formats a render pipeline is made for: a colour format (or none), and a depth
/// format (or none).
type Targets = (Option<wgpu::TextureFormat>, Option<wgpu::TextureFormat>);

struct Pipeline {
    name: String,
    kind: Kind,
    layout: wgpu::BindGroupLayout,
    /// The uniform block's binding and size, if it has one.
    uniform: Option<(u32, u32)>,
    bindings: Vec<ResourceBinding>,
    /// A debug build's: where its bounds checks' flag is bound.
    debug_flag: Option<u32>,
}

enum Kind {
    Compute(wgpu::ComputePipeline),
    Render {
        module: wgpu::ShaderModule,
        pipeline_layout: wgpu::PipelineLayout,
        vertex: String,
        fragment: String,
        /// Its colour is drawn over the target's (the manifest's `blend`).
        blend: bool,
        /// The manifest's `cull`, `depth_bias` and `depth`.
        cull: Cull,
        depth_bias: DepthBias,
        depth: DepthState,
        variants: HashMap<Targets, wgpu::RenderPipeline>,
    },
}

/// A cached bind group: its pipeline, and the bindings it holds.
type BindGroupKey = (u32, Box<[Binding]>);

enum DrawHow {
    Direct {
        vertices: u32,
        instances: u32,
    },
    Indirect {
        buffer: u32,
        offset: u32,
    },
    /// `u32` indices: `size` bytes at `at` in buffer `indices`.
    IndexedIndirect {
        indices: u32,
        at: u32,
        size: u32,
        buffer: u32,
        offset: u32,
    },
}

struct PendingDraw {
    pipeline: u32,
    how: DrawHow,
    bindings: Vec<Binding>,
    uniforms: Vec<u8>,
}

/// A pass between its beginning and `Present` or `EndPass`, and the name the program gave it.
struct OpenPass {
    pass: Pass,
    draws: Vec<PendingDraw>,
    label: Option<String>,
}

struct Screen {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

/// A buffer whose bytes for this submission are staged on the CPU and written to it at the
/// flush: the uniform ring, and the upload ring.
///
/// The upload ring holds this submission's buffer writes (`WriteBuffer`): a copy of each from
/// there is recorded where the write is, so a write splits no work into submissions. (A queue
/// write would have to come between two: the work recorded before it must see the old
/// contents. Each submission waited for the GPU, which then started cold, and the pass after a
/// write measured a third more, the herd's culling 15 times.)
struct Ring {
    label: &'static str,
    buffer: wgpu::Buffer,
    staging: Vec<u8>,
}

impl Ring {
    fn new(
        device: &wgpu::Device,
        label: &'static str,
        usage: wgpu::BufferUsages,
        size: u64,
    ) -> Ring {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage,
            mapped_at_creation: false,
        });
        Ring { label, buffer, staging: Vec::new() }
    }

    fn capacity(&self) -> u64 {
        self.buffer.size()
    }

    /// Replaces the buffer with one of `size` bytes, after a flush (nothing is staged).
    fn grow(&mut self, device: &wgpu::Device, size: u64) {
        debug_assert!(self.staging.is_empty());
        let staging = std::mem::take(&mut self.staging);
        *self = Ring { staging, ..Ring::new(device, self.label, self.buffer.usage(), size) };
    }

    /// Writes this submission's staged bytes to the buffer.
    fn write(&mut self, queue: &wgpu::Queue) {
        if !self.staging.is_empty() {
            queue.write_buffer(&self.buffer, 0, &self.staging);
            self.staging.clear();
        }
    }
}

struct Timer {
    queries: wgpu::QuerySet,
    /// What each pair of queries in this submission times: (frame, label).
    pending: Vec<(usize, String)>,
    results: Vec<GpuTiming>,
    /// Each pass and dispatch submitted and waited for alone, so none overlaps another: each
    /// one's own time (the serial mode).
    serial: bool,
}

pub(crate) struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    limits: wgpu::Limits,
    align: u64,
    pipelines: Vec<Pipeline>,
    /// Each pipeline's manifest entry and WGSL, as text: a hot reload keeps a pipeline whose
    /// both are the same.
    pipeline_keys: Vec<String>,
    resources: HashMap<u32, Resource>,
    /// Each pipeline's cached bind groups, by the bindings they hold.
    bind_groups: Vec<HashMap<Box<[Binding]>, wgpu::BindGroup>>,
    /// For each resource, the cached bind groups it's in: destroying it drops them.
    bound_in: HashMap<u32, HashSet<BindGroupKey>>,
    /// The uniform ring, bound with dynamic offsets, and the upload ring.
    ring: Ring,
    upload: Ring,
    encoder: Option<wgpu::CommandEncoder>,
    pass: Option<OpenPass>,
    /// A pass that ended, not yet recorded: the next pass continues it if it begins on the same
    /// targets and keeps them (`Pass::joins`).
    ended: Option<OpenPass>,
    screen: Option<Screen>,
    timer: Option<Timer>,
    /// The name the program gave the passes and dispatches after it in this frame (`Label`).
    label: Option<String>,
    errors: Arc<Mutex<Vec<String>>>,
    frame: usize,
    /// A debug build's: the flag its pipelines' bounds checks set (the manifest's
    /// `debug_flag`), read after each frame.
    debug_flag: Option<DebugFlag>,
}

/// The flag a debug build's bounds checks set, and the buffer it's read back through, made
/// once.
struct DebugFlag {
    buffer: wgpu::Buffer,
    readback: wgpu::Buffer,
}

/// The limits a program sees, from wgpu's.
pub(crate) fn limits_of(l: &wgpu::Limits) -> wrela_abi::Limits {
    let clamp = |x: u64| u32::try_from(x).unwrap_or(u32::MAX);
    wrela_abi::Limits {
        max_texture_size: l.max_texture_dimension_2d,
        max_buffer_size: clamp(l.max_buffer_size.min(l.max_storage_buffer_binding_size)),
        max_storage_buffers_per_stage: l.max_storage_buffers_per_shader_stage,
        max_uniform_buffer_binding_size: clamp(l.max_uniform_buffer_binding_size),
        max_workgroup_storage_size: l.max_compute_workgroup_storage_size,
        max_workgroup_invocations: l.max_compute_invocations_per_workgroup,
        max_workgroup_size: [
            l.max_compute_workgroup_size_x,
            l.max_compute_workgroup_size_y,
            l.max_compute_workgroup_size_z,
        ],
        max_workgroups_per_dimension: l.max_compute_workgroups_per_dimension,
    }
}

fn gpu_err(e: impl std::fmt::Display) -> Error {
    Error::Gpu(e.to_string())
}

/// Opens the high-performance adapter's device with `features` and the adapter's limits, as the
/// browser runtime asks for them: the program sees the GPU's real limits (`wrela.limit`).
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
        // The adapter's own limits: a program asks for them (`wrela_abi::Limits`).
        required_limits: adapter.limits(),
        ..Default::default()
    }))
    .map_err(|e| Error::Gpu(format!("can't open the device: {e}")))
}

impl Gpu {
    /// Opens the GPU and builds every pipeline. `shaders[i]` is pipeline `i`'s WGSL source.
    /// `timing` says whether passes are timed, and how.
    pub(crate) fn new(manifest: &Manifest, shaders: &[String], timing: Timing) -> Result<Gpu> {
        let timestamps = timing != Timing::Off;
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
        let usage = wgpu::BufferUsages::UNIFORM
            | wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_DST;
        let ring = Ring::new(&device, "uniform ring", usage, RING_START);
        let usage = wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
        let upload = Ring::new(&device, "upload ring", usage, u64::from(stream::UPLOAD_START));
        let timer = timestamps.then(|| Timer::new(&device, timing == Timing::Serial));
        let mut gpu = Gpu {
            device,
            queue,
            limits,
            align,
            pipelines: Vec::new(),
            pipeline_keys: Vec::new(),
            resources: HashMap::new(),
            bind_groups: vec![HashMap::new(); manifest.pipelines.len()],
            bound_in: HashMap::new(),
            ring,
            upload,
            encoder: None,
            pass: None,
            ended: None,
            screen: None,
            timer,
            label: None,
            errors,
            frame: 0,
            debug_flag: None,
        };
        gpu.take_pipelines(manifest, shaders)?;
        Ok(gpu)
    }

    /// Builds the manifest's pipelines, keeping each of the ones it has whose manifest entry
    /// and WGSL are the same.
    fn take_pipelines(&mut self, manifest: &Manifest, shaders: &[String]) -> Result<()> {
        if self.debug_flag.is_none() && manifest.pipelines.iter().any(|p| p.debug_flag.is_some()) {
            self.debug_flag = Some(DebugFlag {
                buffer: self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("debug flag"),
                    size: 4,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                readback: self.readback_buffer(4),
            });
        }
        let old_keys = std::mem::take(&mut self.pipeline_keys);
        let mut old: HashMap<String, Pipeline> =
            old_keys.into_iter().zip(std::mem::take(&mut self.pipelines)).collect();
        for (p, source) in manifest.pipelines.iter().zip(shaders) {
            let key = format!("{p:?}\n{source}");
            let pipeline = match old.remove(&key) {
                Some(kept) => kept,
                None => self.build_pipeline(p, source)?,
            };
            self.pipelines.push(pipeline);
            self.pipeline_keys.push(key);
        }
        self.bind_groups = vec![HashMap::new(); manifest.pipelines.len()];
        self.check_errors()
    }

    /// Hot reload: finishes the old build's work, forgets its resources, and takes the new
    /// build's pipelines. The device, the screen and the rings stay.
    fn reload_build(&mut self, manifest: &Manifest, shaders: &[String]) -> Result<()> {
        self.pass = None;
        self.label = None;
        self.flush()?;
        self.collect()?;
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(gpu_err)?;
        self.resources.clear();
        self.bound_in.clear();
        self.take_pipelines(manifest, shaders)
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
        if let Some(binding) = p.debug_flag {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding,
                // WebGPU forbids writable storage in vertex shaders (which aren't checked).
                visibility: if is_compute { all } else { wgpu::ShaderStages::FRAGMENT },
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            });
        }
        for b in &p.bindings {
            // A render pipeline's binding one shader reads is that shader's alone, so each
            // stage counts only its own against WebGPU's limits.
            let all = match b.stage {
                BindingStage::Both => all,
                BindingStage::Vertex => wgpu::ShaderStages::VERTEX,
                BindingStage::Fragment => wgpu::ShaderStages::FRAGMENT,
            };
            let (ty, visibility) = match b.kind {
                BindingKind::Read | BindingKind::ReadWrite => {
                    let read_only = b.kind == BindingKind::Read;
                    // WebGPU forbids writable storage in vertex shaders.
                    let visibility =
                        if read_only || is_compute { all } else { wgpu::ShaderStages::FRAGMENT };
                    let ty = wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    };
                    (ty, visibility)
                }
                BindingKind::Texture | BindingKind::DepthTexture | BindingKind::Texture3d => {
                    let sample_type = if b.kind == BindingKind::DepthTexture {
                        wgpu::TextureSampleType::Depth
                    } else {
                        sample_type(colour_format(b))
                    };
                    let ty = wgpu::BindingType::Texture {
                        sample_type,
                        view_dimension: view_dimension(b.kind),
                        multisampled: false,
                    };
                    (ty, all)
                }
                BindingKind::Sampler => {
                    (wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), all)
                }
                BindingKind::ComparisonSampler => {
                    (wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison), all)
                }
                // Only a kernel writes a texture's texels.
                BindingKind::StorageTexture | BindingKind::StorageTexture3d => {
                    let ty = wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu_format(colour_format(b)),
                        view_dimension: view_dimension(b.kind),
                    };
                    (ty, wgpu::ShaderStages::COMPUTE)
                }
            };
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: b.binding,
                visibility,
                ty,
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
            Stage::Render {
                vertex_entry,
                fragment_entry,
                blend,
                cull,
                depth_bias,
                depth,
                writes_depth,
                uint,
            } => {
                let mut kind = Kind::Render {
                    module,
                    pipeline_layout,
                    vertex: vertex_entry.clone(),
                    fragment: fragment_entry.clone(),
                    blend: *blend,
                    cull: *cull,
                    depth_bias: *depth_bias,
                    depth: *depth,
                    variants: HashMap::new(),
                };
                // The screen's variant now (an integer target's, for a `u32` output), so a
                // shader's errors come at load (with a depth target, for one that writes its
                // fragments' depth).
                let depth = writes_depth.then_some(wgpu::TextureFormat::Depth32Float);
                let colour = if *uint { wgpu::TextureFormat::R32Uint } else { SCREEN_FORMAT };
                render_variant(&self.device, &p.name, &mut kind, (Some(colour), depth));
                kind
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
            bindings: p.bindings.clone(),
            debug_flag: p.debug_flag,
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

    /// A frame begins: no label carries over from the last one.
    pub(crate) fn set_frame(&mut self, frame: usize) {
        self.frame = frame;
        self.label = None;
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

    fn buffer(&self, handle: u32) -> &wgpu::Buffer {
        match &self.resources[&handle] {
            Resource::Buffer(b) => b,
            _ => unreachable!("the checker checked {handle} is a buffer"),
        }
    }

    /// Bind group 0 of `pipeline` with these bindings, from the cache or made now.
    fn bind_group(&mut self, pipeline: u32, bindings: &[Binding]) -> wgpu::BindGroup {
        if let Some(bg) = self.bind_groups[pipeline as usize].get(bindings) {
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
        for (b, slot) in bindings.iter().zip(&p.bindings) {
            let resource = match &self.resources[&b.handle] {
                Resource::Buffer(buffer) => wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer,
                    offset: u64::from(b.offset),
                    size: NonZeroU64::new(u64::from(b.size)),
                }),
                Resource::Texture { view, .. } => wgpu::BindingResource::TextureView(view),
                Resource::Sampler(s) => wgpu::BindingResource::Sampler(s),
            };
            entries.push(wgpu::BindGroupEntry { binding: slot.binding, resource });
        }
        if let (Some(binding), Some(flag)) = (p.debug_flag, &self.debug_flag) {
            let resource = flag.buffer.as_entire_binding();
            entries.push(wgpu::BindGroupEntry { binding, resource });
        }
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&p.name),
            layout: &p.layout,
            entries: &entries,
        });
        let key: Box<[Binding]> = bindings.into();
        for b in bindings {
            self.bound_in.entry(b.handle).or_default().insert((pipeline, key.clone()));
        }
        self.bind_groups[pipeline as usize].insert(key, bg.clone());
        bg
    }

    /// Forgets a destroyed resource, and the cached bind groups it's in. Work already recorded
    /// holds its own references until the GPU is done.
    fn destroy(&mut self, handle: u32) {
        self.resources.remove(&handle);
        for group in self.bound_in.remove(&handle).unwrap_or_default() {
            for other in group.1.iter().filter(|b| b.handle != handle) {
                if let Some(groups) = self.bound_in.get_mut(&other.handle) {
                    groups.remove(&group);
                }
            }
            self.bind_groups[group.0 as usize].remove(&group.1);
        }
    }

    /// Makes room for `bytes` more uniform bytes (counted with alignment) in this submission,
    /// flushing first or growing the ring if it must. Call before recording the work that uses
    /// them, since a flush submits what's recorded.
    fn reserve_uniforms(&mut self, opcode: Opcode, bytes: u64) -> Result<()> {
        let start = align_to(self.ring.staging.len() as u64, self.align);
        if start + bytes <= self.ring.capacity() {
            return Ok(());
        }
        self.flush()?;
        if bytes > self.ring.capacity() {
            let capacity = bytes.max(self.ring.capacity() * 2).next_power_of_two();
            if capacity > self.limits.max_buffer_size {
                return Err(Error::command(
                    opcode,
                    format!("{bytes} uniform bytes in one submission is over the limit"),
                ));
            }
            self.ring.grow(&self.device, capacity);
            // Cached bind groups point at the old ring.
            self.bind_groups.iter_mut().for_each(HashMap::clear);
            self.bound_in.clear();
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
        debug_assert!(offset + bytes.len() as u64 <= self.ring.capacity());
        self.ring.staging.resize(offset as usize, 0);
        self.ring.staging.extend_from_slice(bytes);
        Some(offset as u32)
    }

    fn aligned(&self, len: usize) -> u64 {
        align_to(len as u64, self.align)
    }

    /// The pending command encoder, taken out so a pass can record into it while `self` is
    /// borrowed. Put it back (`self.encoder = Some(..)`) before anything can flush.
    fn take_encoder(&mut self) -> wgpu::CommandEncoder {
        self.encoder.take().unwrap_or_else(|| {
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default())
        })
    }

    /// Submits everything recorded so far, after writing its uniform bytes (a pass that ended
    /// and waits is recorded first).
    pub(crate) fn flush(&mut self) -> Result<()> {
        self.record_ended()?;
        let Some(encoder) = self.encoder.take() else {
            debug_assert!(self.ring.staging.is_empty() && self.upload.staging.is_empty());
            return Ok(());
        };
        self.ring.write(&self.queue);
        self.upload.write(&self.queue);
        self.queue.submit([encoder.finish()]);
        // The serial mode reads its timestamps once a frame (`collect`), so nothing waits
        // between its submissions and the GPU keeps its clocks.
        if self.timer.as_ref().is_some_and(|t| !t.serial) {
            self.collect()?;
        }
        Ok(())
    }

    /// Reads back the timestamps of the submissions made since the last call.
    fn collect(&mut self) -> Result<()> {
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
            self.collect()?;
        }
        Ok(())
    }

    fn create_buffer(&mut self, handle: u32, size: u32) {
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("buffer {handle}")),
            size: u64::from(size),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::INDEX,
            mapped_at_creation: false,
        });
        self.resources.insert(handle, Resource::Buffer(buffer));
    }

    /// A texture `size[0]` × `size[1]`, and `size[2]` deep for a 3D one (0 for a 2D one). A 3D
    /// one is never a pass's target.
    fn create_texture(
        &mut self,
        handle: u32,
        size: [u32; 3],
        format: TextureFormat,
        writable: bool,
    ) {
        let [width, height, depth] = size;
        let three = depth > 0;
        let mut usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC;
        if !three {
            usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
        }
        if !format.is_depth() {
            usage |= wgpu::TextureUsages::COPY_DST;
        }
        // Only where kernels write it: a storage texture may give up the GPU's compression of
        // what's drawn into it.
        if writable {
            usage |= wgpu::TextureUsages::STORAGE_BINDING;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("texture {handle}")),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: depth.max(1) },
            mip_level_count: 1,
            sample_count: 1,
            dimension: if three { wgpu::TextureDimension::D3 } else { wgpu::TextureDimension::D2 },
            format: wgpu_format(format),
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.resources.insert(handle, Resource::Texture { view, texture, format });
    }

    fn create_sampler(
        &mut self,
        handle: u32,
        linear: bool,
        repeat: bool,
        compare: Option<Compare>,
    ) {
        let filter = if linear { wgpu::FilterMode::Linear } else { wgpu::FilterMode::Nearest };
        let address =
            if repeat { wgpu::AddressMode::Repeat } else { wgpu::AddressMode::ClampToEdge };
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(&format!("sampler {handle}")),
            address_mode_u: address,
            address_mode_v: address,
            address_mode_w: address,
            mag_filter: filter,
            min_filter: filter,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            compare: compare.map(wgpu_compare),
            ..Default::default()
        });
        self.resources.insert(handle, Resource::Sampler(sampler));
    }

    /// Records a copy from the upload ring ([`Ring`]): the stream's writes are whole words, as a
    /// copy moves. A very big one goes through the queue after what's recorded is submitted.
    fn write_buffer(&mut self, handle: u32, offset: u32, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let len = data.len() as u64;
        if len > u64::from(stream::UPLOAD_MAX) {
            // Work recorded before this write must see the old contents.
            self.flush()?;
            self.queue.write_buffer(self.buffer(handle), u64::from(offset), data);
            return Ok(());
        }
        if self.upload.staging.len() as u64 + len > self.upload.capacity() {
            self.flush()?;
            if len > self.upload.capacity() {
                let capacity = len.max(self.upload.capacity() * 2).next_power_of_two();
                self.upload.grow(&self.device, capacity);
            }
        }
        let at = self.upload.staging.len() as u64;
        self.upload.staging.extend_from_slice(data);
        let mut encoder = self.take_encoder();
        encoder.copy_buffer_to_buffer(
            &self.upload.buffer,
            at,
            self.buffer(handle),
            u64::from(offset),
            len,
        );
        self.encoder = Some(encoder);
        Ok(())
    }

    fn write_texture(
        &mut self,
        handle: u32,
        origin: [u32; 2],
        size: [u32; 2],
        data: &[u8],
    ) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.flush()?;
        let Resource::Texture { texture, format, .. } = &self.resources[&handle] else {
            unreachable!("the checker checked {handle} is a texture");
        };
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: origin[0], y: origin[1], z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size[0] * format.bytes_per_texel()),
                rows_per_image: Some(size[1]),
            },
            wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
        );
        Ok(())
    }

    fn copy_buffer(&mut self, from: u32, from_offset: u32, to: u32, to_offset: u32, size: u32) {
        let mut encoder = self.take_encoder();
        encoder.copy_buffer_to_buffer(
            self.buffer(from),
            u64::from(from_offset),
            self.buffer(to),
            u64::from(to_offset),
            u64::from(size),
        );
        self.encoder = Some(encoder);
    }

    fn dispatch(
        &mut self,
        pipeline: u32,
        groups: Result<[u32; 3], (u32, u32)>,
        bindings: &[Binding],
        uniforms: &[u8],
    ) -> Result<()> {
        // Everything that can flush happens before anything is recorded for this dispatch.
        self.reserve_uniforms(Opcode::Dispatch, self.aligned(uniforms.len()))?;
        self.reserve_timestamps()?;
        let offset = self.push_uniforms(uniforms);
        let bind_group = self.bind_group(pipeline, bindings);
        let mut encoder = self.take_encoder();
        let p = &self.pipelines[pipeline as usize];
        let label = self.label.clone();
        let timestamps =
            self.timer.as_mut().map(|t| t.next(self.frame, label.as_deref().unwrap_or(&p.name)));
        let Kind::Compute(compute) = &p.kind else {
            unreachable!("the checker checked the kind");
        };
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(&p.name),
            timestamp_writes: timestamps.zip(self.timer.as_ref()).map(|(i, t)| {
                wgpu::ComputePassTimestampWrites {
                    query_set: &t.queries,
                    beginning_of_pass_write_index: Some(i),
                    end_of_pass_write_index: Some(i + 1),
                }
            }),
        });
        pass.set_pipeline(compute);
        pass.set_bind_group(0, &bind_group, offset.as_slice());
        match groups {
            Ok([x, y, z]) => pass.dispatch_workgroups(x, y, z),
            Err((buffer, at)) => {
                pass.dispatch_workgroups_indirect(self.buffer(buffer), u64::from(at))
            }
        }
        drop(pass);
        self.encoder = Some(encoder);
        self.alone()
    }

    /// In the serial mode, submits what's recorded and waits for the GPU to finish it, so the
    /// next pass or dispatch starts alone. (Submissions alone aren't enough: on Apple GPUs a
    /// render pass overlaps the kernel submitted before it.) Its timestamps are read once a
    /// frame, so only the wait comes between the submissions.
    fn alone(&mut self) -> Result<()> {
        if self.timer.as_ref().is_some_and(|t| t.serial) {
            self.flush()?;
            self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(gpu_err)?;
        }
        Ok(())
    }

    fn draw(&mut self, pipeline: u32, how: DrawHow, bindings: &[Binding], uniforms: &[u8]) {
        let draw =
            PendingDraw { pipeline, how, bindings: bindings.to_vec(), uniforms: uniforms.to_vec() };
        self.pass.as_mut().expect("the sequencer checked a pass is open").draws.push(draw);
    }

    /// Ends the open pass: on the screen (`Present`) it's recorded; into textures (`EndPass`),
    /// it waits for the next command, which may continue it (`Pass::joins`), unless the timing is
    /// serial.
    fn end_pass(&mut self, op: Opcode) -> Result<()> {
        let Some(open) = self.pass.take() else {
            unreachable!("the sequencer checked a pass is open");
        };
        let serial = self.timer.as_ref().is_some_and(|t| t.serial);
        if op == Opcode::EndPass && !serial {
            self.ended = Some(open);
            return Ok(());
        }
        self.record_pass(op, open)
    }

    /// Records the pass that ended and waits (`ended`), if there is one.
    fn record_ended(&mut self) -> Result<()> {
        match self.ended.take() {
            Some(open) => self.record_pass(Opcode::EndPass, open),
            None => Ok(()),
        }
    }

    /// Records a pass's draws into one render pass.
    fn record_pass(&mut self, op: Opcode, open: OpenPass) -> Result<()> {
        let pass = open.pass;
        let on_screen = pass.color == stream::SCREEN;
        if on_screen && self.screen.is_none() {
            return Err(Error::command(
                op,
                "there's no screen: the host draws only while running frames",
            ));
        }
        let format_of = |gpu: &Gpu, h: u32| match &gpu.resources[&h] {
            Resource::Texture { format, .. } => wgpu_format(*format),
            _ => unreachable!("the checker checked {h} is a texture"),
        };
        let color_format = match pass.color {
            stream::SCREEN => Some(SCREEN_FORMAT),
            stream::NONE => None,
            h => Some(format_of(self, h)),
        };
        let depth_format = (pass.depth != stream::NONE).then(|| format_of(self, pass.depth));
        let targets = (color_format, depth_format);
        // Every pipeline variant the pass needs, made before anything is recorded. (The checker
        // checked each draw's pipeline is one these targets take.)
        for d in &open.draws {
            let Pipeline { name, kind, .. } = &mut self.pipelines[d.pipeline as usize];
            render_variant(&self.device, name, kind, targets);
        }
        // Everything that can flush happens before anything is recorded for this pass.
        let total = open.draws.iter().map(|d| self.aligned(d.uniforms.len())).sum();
        self.reserve_uniforms(op, total)?;
        self.reserve_timestamps()?;
        let label = open.label.as_deref().unwrap_or(if on_screen { "screen pass" } else { "pass" });
        let timestamps = self.timer.as_mut().map(|t| t.next(self.frame, label));
        let [r, g, b, a] = pass.clear.map(f64::from);
        let mut bind_groups = Vec::new();
        let mut offsets = Vec::new();
        for d in &open.draws {
            offsets.push(self.push_uniforms(&d.uniforms));
            bind_groups.push(self.bind_group(d.pipeline, &d.bindings));
        }
        let mut encoder = self.take_encoder();
        let view_of = |gpu: &'_ Gpu, h: u32| -> wgpu::TextureView {
            match &gpu.resources[&h] {
                Resource::Texture { view, .. } => view.clone(),
                _ => unreachable!("the checker checked {h} is a texture"),
            }
        };
        let color_view = match pass.color {
            stream::SCREEN => Some(self.screen.as_ref().expect("checked above").view.clone()),
            stream::NONE => None,
            h => Some(view_of(self, h)),
        };
        let depth_view = (pass.depth != stream::NONE).then(|| view_of(self, pass.depth));
        let color_attachment = color_view.as_ref().map(|view| wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: if pass.keep_color {
                    wgpu::LoadOp::Load
                } else {
                    wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a })
                },
                store: wgpu::StoreOp::Store,
            },
        });
        let depth_attachment =
            depth_view.as_ref().map(|view| wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    load: if pass.keep_depth {
                        wgpu::LoadOp::Load
                    } else {
                        wgpu::LoadOp::Clear(pass.clear_depth)
                    },
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            });
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[color_attachment],
            depth_stencil_attachment: depth_attachment,
            timestamp_writes: timestamps.zip(self.timer.as_ref()).map(|(i, t)| {
                wgpu::RenderPassTimestampWrites {
                    query_set: &t.queries,
                    beginning_of_pass_write_index: Some(i),
                    end_of_pass_write_index: Some(i + 1),
                }
            }),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        for ((d, bind_group), offset) in open.draws.iter().zip(&bind_groups).zip(&offsets) {
            let Kind::Render { variants, .. } = &self.pipelines[d.pipeline as usize].kind else {
                unreachable!("the checker checked the kind");
            };
            rp.set_pipeline(&variants[&targets]);
            rp.set_bind_group(0, bind_group, offset.as_slice());
            match d.how {
                DrawHow::Direct { vertices, instances } => rp.draw(0..vertices, 0..instances),
                DrawHow::Indirect { buffer, offset } => {
                    rp.draw_indirect(self.buffer(buffer), u64::from(offset));
                }
                DrawHow::IndexedIndirect { indices, at, size, buffer, offset } => {
                    let range = u64::from(at)..u64::from(at) + u64::from(size);
                    rp.set_index_buffer(
                        self.buffer(indices).slice(range),
                        wgpu::IndexFormat::Uint32,
                    );
                    rp.draw_indexed_indirect(self.buffer(buffer), u64::from(offset));
                }
            }
        }
        drop(rp);
        self.encoder = Some(encoder);
        if on_screen { self.flush() } else { self.alone() }
    }

    /// A buffer's bytes `offset..offset + size`, copied back to the CPU (waits for the GPU).
    pub(crate) fn read_range(&mut self, handle: u32, offset: u32, size: u32) -> Result<Vec<u8>> {
        self.flush()?;
        let Some(Resource::Buffer(buffer)) = self.resources.get(&handle) else {
            return Err(Error::Gpu(format!("there's no buffer {handle}")));
        };
        let staging = self.readback_buffer(u64::from(size.max(4)));
        let bytes = self.copy_back(buffer, u64::from(offset), u64::from(size), &staging)?;
        self.check_errors()?;
        Ok(bytes)
    }

    /// Copies `source`'s bytes `offset..offset + size` to the CPU through `staging`, a
    /// readback buffer at least `size` bytes long (waits for the GPU).
    fn copy_back(
        &self,
        source: &wgpu::Buffer,
        offset: u64,
        size: u64,
        staging: &wgpu::Buffer,
    ) -> Result<Vec<u8>> {
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(source, offset, staging, 0, size);
        self.queue.submit([encoder.finish()]);
        let mut bytes = map_read(&self.device, staging)?;
        bytes.truncate(size as usize);
        Ok(bytes)
    }

    /// The handles of the buffers that exist, oldest first: for tests.
    pub(crate) fn buffers(&self) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .resources
            .iter()
            .filter(|(_, r)| matches!(r, Resource::Buffer(_)))
            .map(|(&h, _)| h)
            .collect();
        // Handles are given in order and never reused.
        out.sort_unstable();
        out
    }

    /// Copies a whole buffer back to the CPU (waits for the GPU): for tests.
    pub(crate) fn read_buffer(&mut self, handle: u32) -> Result<Vec<u8>> {
        let size = match self.resources.get(&handle) {
            Some(Resource::Buffer(b)) => b.size() as u32,
            _ => return Err(Error::Gpu(format!("there's no buffer {handle}"))),
        };
        self.read_range(handle, 0, size)
    }

    /// Copies the screen back: RGBA8, rows top to bottom, no padding (waits for the GPU).
    pub(crate) fn read_screen(&mut self) -> Result<Vec<u8>> {
        self.flush()?;
        let screen =
            self.screen.as_ref().ok_or_else(|| Error::Gpu("there's no screen yet".into()))?;
        let (width, height) = (screen.width, screen.height);
        self.read_screen_region([0, 0], [width, height])
    }

    /// Copies the screen's pixels from `corner` (x, y), `size` (width, height) of them, back:
    /// RGBA8, rows top to bottom, no padding (waits for the GPU).
    pub(crate) fn read_screen_region(
        &mut self,
        corner: [u32; 2],
        size: [u32; 2],
    ) -> Result<Vec<u8>> {
        self.flush()?;
        let screen =
            self.screen.as_ref().ok_or_else(|| Error::Gpu("there's no screen yet".into()))?;
        let ([x, y], [width, height]) = (corner, size);
        let inside = |at: u32, n: u32, of: u32| n > 0 && at.checked_add(n).is_some_and(|e| e <= of);
        if !inside(x, width, screen.width) || !inside(y, height, screen.height) {
            return Err(Error::Gpu(format!(
                "{width}x{height} pixels at ({x}, {y}) aren't all on the {}x{} screen",
                screen.width, screen.height
            )));
        }
        let row = u64::from(width) * 4;
        let padded = align_to(row, u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT));
        let staging = self.readback_buffer(padded * u64::from(height));
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &screen.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
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
        if padded == row {
            return Ok(bytes);
        }
        let mut frame = Vec::with_capacity((row * u64::from(height)) as usize);
        for padded_row in bytes.chunks_exact(padded as usize) {
            frame.extend_from_slice(&padded_row[..row as usize]);
        }
        Ok(frame)
    }

    /// A debug build's: an error if a pipeline indexed an array out of range (its number, from
    /// 1, is in the flag). Waits for the GPU.
    fn check_debug_flag(&mut self) -> Result<()> {
        let Some(flag) = &self.debug_flag else { return Ok(()) };
        let bytes = self.copy_back(&flag.buffer, 0, 4, &flag.readback)?;
        let code = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if code == 0 {
            return Ok(());
        }
        let name = match self.pipelines.get(code as usize - 1) {
            Some(p) => format!("`{}`", p.name),
            None => format!("number {code}"),
        };
        Err(Error::Gpu(format!(
            "debug build: pipeline {name} indexed an array out of range (in this frame, or a \
             call before it); WGSL then reads or writes an element in range, or a zero"
        )))
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
        if let Some(ended) = &self.ended {
            match cmd {
                // A label names the passes after it, the next of which may yet continue this one.
                Command::Label { .. } => {}
                Command::BeginPass(next) if next.joins(&ended.pass) => {
                    self.pass = self.ended.take();
                    return Ok(());
                }
                _ => self.record_ended()?,
            }
        }
        match cmd {
            Command::CreateBuffer { handle, size } => self.create_buffer(*handle, *size),
            Command::CreateTexture { handle, width, height, format, writable, depth } => {
                self.create_texture(*handle, [*width, *height, *depth], *format, *writable)
            }
            Command::CreateSampler { handle, linear, repeat, compare } => {
                self.create_sampler(*handle, *linear, *repeat, *compare)
            }
            Command::WriteBuffer { handle, offset, data } => {
                return self.write_buffer(*handle, *offset, data);
            }
            Command::WriteTexture { handle, x, y, width, height, data } => {
                return self.write_texture(*handle, [*x, *y], [*width, *height], data);
            }
            Command::CopyBuffer {
                source,
                source_offset,
                destination,
                destination_offset,
                size,
            } => {
                self.copy_buffer(*source, *source_offset, *destination, *destination_offset, *size)
            }
            Command::Dispatch { pipeline, groups, bindings, uniforms } => {
                return self.dispatch(*pipeline, Ok(*groups), bindings, uniforms);
            }
            Command::DispatchIndirect { pipeline, arguments, offset, bindings, uniforms } => {
                return self.dispatch(*pipeline, Err((*arguments, *offset)), bindings, uniforms);
            }
            Command::BeginScreenPass { clear } => {
                let pass = Pass {
                    color: stream::SCREEN,
                    keep_color: false,
                    join: false,
                    clear: *clear,
                    depth: stream::NONE,
                    keep_depth: false,
                    clear_depth: 1.0,
                };
                let label = self.label.clone();
                self.pass = Some(OpenPass { pass, draws: Vec::new(), label });
            }
            Command::BeginPass(pass) => {
                let label = self.label.clone();
                self.pass = Some(OpenPass { pass: *pass, draws: Vec::new(), label })
            }
            Command::Draw { pipeline, vertices, instances, bindings, uniforms } => {
                let how = DrawHow::Direct { vertices: *vertices, instances: *instances };
                self.draw(*pipeline, how, bindings, uniforms);
            }
            Command::DrawIndirect { pipeline, arguments, offset, bindings, uniforms } => {
                let how = DrawHow::Indirect { buffer: *arguments, offset: *offset };
                self.draw(*pipeline, how, bindings, uniforms);
            }
            Command::DrawIndexedIndirect {
                pipeline,
                indices,
                index_offset,
                index_size,
                arguments,
                offset,
                bindings,
                uniforms,
            } => {
                let how = DrawHow::IndexedIndirect {
                    indices: *indices,
                    at: *index_offset,
                    size: *index_size,
                    buffer: *arguments,
                    offset: *offset,
                };
                self.draw(*pipeline, how, bindings, uniforms);
            }
            Command::Present => return self.end_pass(Opcode::Present),
            Command::EndPass => return self.end_pass(Opcode::EndPass),
            Command::DestroyBuffer { handle }
            | Command::DestroyTexture { handle }
            | Command::DestroySampler { handle } => self.destroy(*handle),
            // Requests are the program's host's business (`program`), not the GPU's.
            Command::ReadBuffer { .. }
            | Command::StorageRead { .. }
            | Command::StorageWrite { .. }
            | Command::Fetch { .. }
            | Command::Post { .. }
            | Command::Log { .. } => {}
            Command::Label { name } => self.label = Some(name.to_string()),
        }
        Ok(())
    }

    fn end_frame(&mut self) -> Result<()> {
        self.flush()?;
        self.collect()?;
        self.check_errors()?;
        self.check_debug_flag()
    }

    fn read_back(&mut self, handle: u32, offset: u32, size: u32) -> Result<Vec<u8>> {
        self.read_range(handle, offset, size)
    }

    fn limits(&self) -> wrela_abi::Limits {
        limits_of(&self.limits)
    }

    fn reload(&mut self, manifest: &Manifest, shaders: &[String]) -> Result<()> {
        self.reload_build(manifest, shaders)
    }
}

/// Makes a render pipeline's variant for `targets`, if it isn't made yet.
fn render_variant(device: &wgpu::Device, name: &str, kind: &mut Kind, targets: Targets) {
    let Kind::Render {
        module,
        pipeline_layout,
        vertex,
        fragment,
        blend,
        cull,
        depth_bias,
        depth,
        variants,
        ..
    } = kind
    else {
        return;
    };
    if variants.contains_key(&targets) {
        return;
    }
    // Straight alpha, over what's there (the manifest's `blend`).
    let over = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::SrcAlpha,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
    };
    let color = targets.0.map(|format| wgpu::ColorTargetState {
        format,
        blend: blend.then_some(over),
        write_mask: wgpu::ColorWrites::ALL,
    });
    let depth_stencil = targets.1.map(|format| wgpu::DepthStencilState {
        format,
        depth_write_enabled: Some(depth.write),
        depth_compare: Some(wgpu_compare(depth.compare)),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState {
            constant: depth_bias.constant,
            slope_scale: depth_bias.slope_scale,
            clamp: depth_bias.clamp,
        },
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(name),
        layout: Some(pipeline_layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(vertex),
            compilation_options: Default::default(),
            buffers: &[],
        },
        // Triangle list, counter-clockwise front faces: WebGPU's defaults.
        primitive: wgpu::PrimitiveState {
            cull_mode: match cull {
                Cull::None => None,
                Cull::Front => Some(wgpu::Face::Front),
                Cull::Back => Some(wgpu::Face::Back),
            },
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fragment),
            compilation_options: Default::default(),
            targets: &[color],
        }),
        multiview_mask: None,
        cache: None,
    });
    variants.insert(targets, pipeline);
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
    fn new(device: &wgpu::Device, serial: bool) -> Timer {
        Timer {
            serial,
            queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: TIMESTAMP_QUERIES,
            }),
            pending: Vec::new(),
            results: Vec::new(),
        }
    }

    /// Notes a pass about to be timed; returns the first of its two query indices.
    /// [`Gpu::reserve_timestamps`] must have made room.
    fn next(&mut self, frame: usize, label: &str) -> u32 {
        self.pending.push((frame, label.to_string()));
        (self.pending.len() as u32 - 1) * 2
    }

    /// Reads back the timestamps of the submissions made since the last call; waits for the GPU.
    /// The span mode calls it at every flush, so it measures GPU durations, not throughput; the
    /// serial mode, which waits after each pass anyway, once a frame.
    fn collect(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<()> {
        let times = read_timestamp_spans(device, queue, &self.queries, self.pending.len() as u32)?;
        for ((frame, label), (start, end)) in self.pending.drain(..).zip(times) {
            self.results.push(GpuTiming { frame, label, nanos: end - start, start, end });
        }
        Ok(())
    }
}

/// The GPU durations of `passes` timed passes, in nanoseconds. Pass `i` wrote queries `2i`
/// (start) and `2i + 1` (end) of `queries`, in a submission already made. Waits for the GPU.
///
/// The queries are resolved in a later submission, after the timed one completes: on Apple GPUs
/// (wgpu's Metal backend) a resolve in the same command buffer as the passes it times can read
/// the previous submission's values.
pub fn read_timestamps(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    queries: &wgpu::QuerySet,
    passes: u32,
) -> Result<Vec<f64>> {
    let spans = read_timestamp_spans(device, queue, queries, passes)?;
    Ok(spans.into_iter().map(|(start, end)| end - start).collect())
}

/// [`read_timestamps`]' passes' starts and ends, in nanoseconds on the GPU's clock (an end
/// before its start, as a clock that wrapped, is taken as the start).
fn read_timestamp_spans(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    queries: &wgpu::QuerySet,
    passes: u32,
) -> Result<Vec<(f64, f64)>> {
    if passes == 0 {
        return Ok(Vec::new());
    }
    device.poll(wgpu::PollType::wait_indefinitely()).map_err(gpu_err)?;
    let n = passes * 2;
    let size = u64::from(n) * 8;
    let buffer = |label, usage| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage,
            mapped_at_creation: false,
        })
    };
    let resolve =
        buffer("timestamps", wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let readback =
        buffer("timestamps readback", wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.resolve_query_set(queries, 0..n, &resolve, 0);
    encoder.copy_buffer_to_buffer(&resolve, 0, &readback, 0, size);
    queue.submit([encoder.finish()]);
    let bytes = map_read(device, &readback)?;
    let tick = |b: &[u8]| u64::from_le_bytes(b.try_into().expect("8 bytes"));
    let period = f64::from(queue.get_timestamp_period());
    Ok(bytes
        .chunks_exact(16)
        .map(|pair| {
            let (a, b) = (tick(&pair[..8]), tick(&pair[8..]));
            (a as f64 * period, a.max(b) as f64 * period)
        })
        .collect())
}
