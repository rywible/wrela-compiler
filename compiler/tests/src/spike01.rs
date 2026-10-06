//! Spike 01's harness, ported to Rust (#42, "The harness is new code"): its kernels and its
//! `grazer.js`, unchanged, run the way its `main.js` runs them. `encodeExtract`, `extractHerd`,
//! `update` and `encodeFrame` are ported here; the numbers that come from `grazer.js` (each
//! seed's parameters and bounds, the herd's placement, the scenes' views and the bone palettes)
//! come from it, run by bun (compiler/tests/spike01/data.mjs). The spike ran in Chrome; this runs
//! on the native host's wgpu, so the herd's own build can run beside it in one process
//! (M1's method: alternating, after a warm-up).
//!
//! Its frames are drawn as the spike's are: into an `rgba8unorm-srgb` target, then `blit.wgsl`
//! raises each value to 1/2.2 into the canvas (here an `rgba8unorm` texture, read back).

use crate::repo_root;
use std::sync::OnceLock;
use wgpu::util::DeviceExt;

/// The spike's screen, and its herd.
pub const W: u32 = 1920;
pub const H: u32 = 1080;
pub const HERD: usize = 40;
const BONES: usize = 29;
const PALETTE_STRIDE: u64 = 2048;
const TERRAIN_VERTS: u32 = 256 * 256 * 6;
/// Sketch 02's root iterations and the 6 cm skin falloff.
const ITERS: u32 = 4;
const FALLOFF: f32 = 0.06;

/// One seed's numbers, as `makeGrazer` gives them.
#[derive(Clone, Debug)]
pub struct Seed {
    pub seed: u32,
    /// The `Grazer` uniform: 328 floats.
    pub params: Vec<f32>,
    /// Its rest-space bounds, in f64 as JavaScript computed them.
    pub min: [f64; 3],
    pub max: [f64; 3],
}

/// Where a grazer stands (main.js's instances).
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f64,
    pub phase: f64,
    pub head_down: f64,
}

/// A scene: the herd (seeds 1 to 40, as placed) or the close-up (seed 1 at the origin).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneKind {
    Herd,
    CloseUp,
}

impl SceneKind {
    fn name(self) -> &'static str {
        match self {
            SceneKind::Herd => "herd",
            SceneKind::CloseUp => "closeup",
        }
    }
}

/// Everything `grazer.js` gives: the seeds, the placements, and each scene's `View` uniform at
/// t = 0 (its fourth vec4's w is t).
pub struct SpikeData {
    pub seeds: Vec<Seed>,
    pub herd: Vec<Placed>,
    pub closeup: Vec<Placed>,
    pub herd_view: Vec<f32>,
    pub closeup_view: Vec<f32>,
}

fn bun(args: &[&str]) -> Vec<u8> {
    let dir = repo_root().join("compiler/tests/spike01");
    let out = std::process::Command::new("bun")
        .arg("data.mjs")
        .args(args)
        .current_dir(&dir)
        .output()
        .expect("bun runs compiler/tests/spike01/data.mjs");
    assert!(out.status.success(), "data.mjs failed: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// `grazer.js`'s numbers, once per process.
pub fn data() -> &'static SpikeData {
    static DATA: OnceLock<SpikeData> = OnceLock::new();
    DATA.get_or_init(|| {
        let v: serde_json::Value = serde_json::from_slice(&bun(&["seeds"])).expect("JSON");
        let f = |x: &serde_json::Value| x.as_f64().expect("a number");
        let v3 = |x: &serde_json::Value| {
            let a = x.as_array().expect("a vector");
            [f(&a[0]), f(&a[1]), f(&a[2])]
        };
        let placed = |x: &serde_json::Value| {
            x.as_array()
                .expect("placements")
                .iter()
                .map(|p| Placed {
                    x: f(&p["x"]),
                    y: f(&p["y"]),
                    z: f(&p["z"]),
                    yaw: f(&p["yaw"]),
                    phase: f(&p["phase"]),
                    head_down: f(&p["headDown"]),
                })
                .collect()
        };
        let floats = |x: &serde_json::Value| {
            x.as_array().expect("floats").iter().map(|n| f(n) as f32).collect::<Vec<f32>>()
        };
        let seeds = v["seeds"]
            .as_array()
            .expect("seeds")
            .iter()
            .map(|s| Seed {
                seed: s["seed"].as_u64().expect("a seed") as u32,
                params: floats(&s["params"]),
                min: v3(&s["min"]),
                max: v3(&s["max"]),
            })
            .collect();
        SpikeData {
            seeds,
            herd: placed(&v["herd"]),
            closeup: placed(&v["closeup"]),
            herd_view: floats(&v["views"]["herd"]),
            closeup_view: floats(&v["views"]["closeup"]),
        }
    })
}

/// Bone palettes for `scene` at each time in `times`: for each time, each instance's 29 mat4s.
pub fn palettes(scene: SceneKind, times: &[f64]) -> Vec<f32> {
    let ts: Vec<String> = times.iter().map(|t| format!("{t}")).collect();
    let mut args = vec!["palettes", scene.name()];
    args.extend(ts.iter().map(String::as_str));
    let bytes = bun(&args);
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

/// A grid for one individual at one cell size (main.js's `gridFor`), in f64 as JavaScript.
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    pub origin: [f64; 3],
    pub cell: f64,
    pub dims: [u32; 3],
    pub blocks: u32,
}

pub fn grid_for(s: &Seed, cell: f64) -> Grid {
    let pad = 2.0 * cell;
    let origin = [s.min[0] - pad, s.min[1] - pad, s.min[2] - pad];
    let dims = [0, 1, 2].map(|a| ((s.max[a] + pad - origin[a]) / (4.0 * cell)).ceil() as u32);
    Grid { origin, cell, dims, blocks: dims[0] * dims[1] * dims[2] }
}

/// One individual's extraction: its counts, and its GPU time per pass (ms).
#[derive(Clone, Copy, Debug, Default)]
pub struct Extracted {
    pub blocks: u32,
    pub live: u32,
    pub verts: u32,
    pub tris: u32,
    pub holes: u32,
    pub flags: u32,
    pub cull_ms: f64,
    pub place_ms: f64,
    pub emit_ms: f64,
}

impl Extracted {
    pub fn total_ms(&self) -> f64 {
        self.cull_ms + self.place_ms + self.emit_ms
    }
}

/// An individual's buffers (main.js's `individual`).
pub struct Individual {
    pub seed: usize,
    pub grid: Grid,
    pub vcap: u32,
    pub icap: u32,
    params: wgpu::Buffer,
    verts: wgpu::Buffer,
    indices: wgpu::Buffer,
    draw: wgpu::Buffer,
    cull_bg: wgpu::BindGroup,
    place_bg: wgpu::BindGroup,
}

/// What every extraction shares (main.js's `scratch`).
struct Scratch {
    live: wgpu::Buffer,
    live_args: wgpu::Buffer,
    live_init: wgpu::Buffer,
    block_map: wgpu::Buffer,
    cells: wgpu::Buffer,
    draw_init: wgpu::Buffer,
}

/// The spike's GPU work, on a device of its own: its pipelines and frame resources.
pub struct Spike {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    cull_layout: wgpu::BindGroupLayout,
    place_layout: wgpu::BindGroupLayout,
    frame_layout: wgpu::BindGroupLayout,
    shadow_frame_layout: wgpu::BindGroupLayout,
    grazer_layout: wgpu::BindGroupLayout,
    blit_layout: wgpu::BindGroupLayout,
    cull: wgpu::ComputePipeline,
    place: wgpu::ComputePipeline,
    emit: wgpu::ComputePipeline,
    shadow: wgpu::RenderPipeline,
    shade: wgpu::RenderPipeline,
    terrain: wgpu::RenderPipeline,
    blit: wgpu::RenderPipeline,
    frame: Option<FrameResources>,
    // Declared last: dropped after the device.
    _lock: wrela_host::lock::GpuLock,
}

struct FrameResources {
    color: wgpu::TextureView,
    depth: wgpu::TextureView,
    shadow: wgpu::TextureView,
    canvas: wgpu::Texture,
    view: wgpu::Buffer,
    palette: wgpu::Buffer,
    frame_bg: wgpu::BindGroup,
    shadow_frame_bg: wgpu::BindGroup,
    blit_bg: wgpu::BindGroup,
}

/// A scene ready to draw (main.js's `scene`).
pub struct Scene<'a> {
    pub kind: SceneKind,
    inds: Vec<&'a Individual>,
    draws: Vec<wgpu::BindGroup>,
    pub tris: u64,
}

/// One frame's GPU times, ms: the shadow pass, the terrain, the creatures' shading, the two
/// creature passes, and the whole frame (first pass's start to the last's end).
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameTimes {
    pub shadow: f64,
    pub terrain: f64,
    pub shade: f64,
    pub creatures: f64,
    /// From the shadow pass's start to the shading pass's end: passes overlap here (on the
    /// native host the terrain's runs beside the shadow pass).
    pub frame: f64,
    /// The three passes' own times summed: the frame's GPU work, however it overlaps.
    pub passes: f64,
}

fn read(device: &wgpu::Device, queue: &wgpu::Queue, src: &wgpu::Buffer, size: u64) -> Vec<u8> {
    let rb = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_buffer_to_buffer(src, 0, &rb, 0, size);
    queue.submit([enc.finish()]);
    wrela_host::map_read(device, &rb).expect("read back")
}

fn u32s(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

/// Each timestamp of a query set, in ns from the first.
fn stamps(device: &wgpu::Device, queue: &wgpu::Queue, qs: &wgpu::QuerySet, n: u32) -> Vec<f64> {
    let size = u64::from(n) * 8;
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("timestamps"),
        size,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.resolve_query_set(qs, 0..n, &resolve, 0);
    queue.submit([enc.finish()]);
    let raw = read(device, queue, &resolve, size);
    let period = f64::from(queue.get_timestamp_period());
    raw.chunks_exact(8)
        .map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")) as f64 * period)
        .collect()
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(repo_root().join("compiler/tests/fixtures/spike01").join(name))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// The salted field (main.js's `salted`): unique source, so no cache serves it.
fn salted(field: &str, salt: Option<f64>) -> String {
    match salt {
        None => field.to_string(),
        Some(s) => field.replace("const SALT: f32 = 0.0;", &format!("const SALT: f32 = {s:.4};")),
    }
}

fn vertex_layout() -> [wgpu::VertexAttribute; 5] {
    [
        wgpu::VertexAttribute {
            shader_location: 0,
            offset: 0,
            format: wgpu::VertexFormat::Float32x3,
        },
        wgpu::VertexAttribute {
            shader_location: 1,
            offset: 12,
            format: wgpu::VertexFormat::Uint8x4,
        },
        wgpu::VertexAttribute {
            shader_location: 2,
            offset: 16,
            format: wgpu::VertexFormat::Float32x3,
        },
        wgpu::VertexAttribute {
            shader_location: 3,
            offset: 28,
            format: wgpu::VertexFormat::Unorm8x4,
        },
        wgpu::VertexAttribute {
            shader_location: 4,
            offset: 32,
            format: wgpu::VertexFormat::Uint32,
        },
    ]
}

impl Spike {
    /// Opens a device (with timestamps) and builds the spike's pipelines: the scene's seven
    /// (cull, place, emit, shadow, field shading without a prepass, terrain) and the blit.
    pub fn new() -> Spike {
        Spike::with_extract(&fixture("extract.wgsl"))
    }

    /// [`Spike::new`], with `extract` in place of the spike's extract.wgsl: for measuring what
    /// a part of its extraction costs.
    pub fn with_extract(extract: &str) -> Spike {
        let lock = wrela_host::lock::GpuLock::acquire("spike 01").expect("the GPU lock");
        let (device, queue) = wrela_host::open_device("spike 01", wgpu::Features::TIMESTAMP_QUERY)
            .expect("a GPU with timestamps");
        Spike::build(device, queue, None, extract, lock)
    }

    /// The spike's extract.wgsl.
    pub fn extract_source() -> String {
        fixture("extract.wgsl")
    }

    fn build(
        device: wgpu::Device,
        queue: wgpu::Queue,
        salt: Option<f64>,
        extract: &str,
        lock: wrela_host::lock::GpuLock,
    ) -> Spike {
        use wgpu::{BindGroupLayoutEntry as E, BindingType as B, ShaderStages as S};
        let buf = |binding, visibility, ty| E {
            binding,
            visibility,
            ty: B::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let storage = wgpu::BufferBindingType::Storage { read_only: false };
        let uniform = wgpu::BufferBindingType::Uniform;
        let extract_layout = |skip: u32| {
            let mut entries = vec![buf(0, S::COMPUTE, uniform), buf(1, S::COMPUTE, uniform)];
            entries.extend((2..9).filter(|b| *b != skip).map(|b| buf(b, S::COMPUTE, storage)));
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &entries,
            })
        };
        let cull_layout = extract_layout(99);
        // live_args is the indirect buffer here, so it can't also be bound.
        let place_layout = extract_layout(3);
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame"),
            entries: &[
                buf(0, S::VERTEX | S::FRAGMENT, uniform),
                E {
                    binding: 1,
                    visibility: S::FRAGMENT,
                    ty: B::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                E {
                    binding: 2,
                    visibility: S::FRAGMENT,
                    ty: B::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                E {
                    binding: 3,
                    visibility: S::FRAGMENT,
                    ty: B::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                E {
                    binding: 4,
                    visibility: S::FRAGMENT,
                    ty: B::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let shadow_frame_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("shadow frame"),
                entries: &[buf(0, S::VERTEX | S::FRAGMENT, uniform)],
            });
        let grazer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("grazer"),
            entries: &[
                buf(0, S::FRAGMENT, uniform),
                buf(1, S::VERTEX, wgpu::BufferBindingType::Storage { read_only: true }),
            ],
        });
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
            entries: &[
                E {
                    binding: 0,
                    visibility: S::FRAGMENT,
                    ty: B::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                E {
                    binding: 1,
                    visibility: S::FRAGMENT,
                    ty: B::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let field = salted(&fixture("field.wgsl"), salt);
        let module = |label: &str, code: String| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(code.into()),
            })
        };
        let em = module("extract", format!("{field}{extract}"));
        let dm = module("draw", format!("{field}{}", fixture("draw.wgsl")));
        let bm = module("blit", fixture("blit.wgsl"));
        let layout = |bgls: &[&wgpu::BindGroupLayout]| {
            let bgls: Vec<Option<&wgpu::BindGroupLayout>> = bgls.iter().map(|b| Some(*b)).collect();
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &bgls,
                immediate_size: 0,
            })
        };
        let compute = |entry: &str, bgl: &wgpu::BindGroupLayout| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout(&[bgl])),
                module: &em,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let cull = compute("cull_blocks", &cull_layout);
        let place = compute("place_vertices", &place_layout);
        let emit = compute("emit_quads", &place_layout);
        let attrs = vertex_layout();
        let vbuf = [Some(wgpu::VertexBufferLayout {
            array_stride: 48,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attrs,
        })];
        let depth = |compare, bias: wgpu::DepthBiasState| wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(compare),
            stencil: Default::default(),
            bias,
        };
        let prim = |cull_mode| wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode,
            ..Default::default()
        };
        let target = |format| {
            Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })
        };
        let srgb = wgpu::TextureFormat::Rgba8UnormSrgb;
        let shadow = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow"),
            layout: Some(&layout(&[&shadow_frame_layout, &grazer_layout])),
            vertex: wgpu::VertexState {
                module: &dm,
                entry_point: Some("skin_shadow"),
                compilation_options: Default::default(),
                buffers: &vbuf,
            },
            primitive: prim(None),
            depth_stencil: Some(depth(
                wgpu::CompareFunction::Less,
                wgpu::DepthBiasState { constant: 4, slope_scale: 2.0, clamp: 0.0 },
            )),
            multisample: Default::default(),
            fragment: None,
            multiview_mask: None,
            cache: None,
        });
        let shade = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shade_field_np"),
            layout: Some(&layout(&[&frame_layout, &grazer_layout])),
            vertex: wgpu::VertexState {
                module: &dm,
                entry_point: Some("skin"),
                compilation_options: Default::default(),
                buffers: &vbuf,
            },
            primitive: prim(Some(wgpu::Face::Back)),
            depth_stencil: Some(depth(wgpu::CompareFunction::Less, Default::default())),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &dm,
                entry_point: Some("shade_field"),
                compilation_options: Default::default(),
                targets: &[target(srgb)],
            }),
            multiview_mask: None,
            cache: None,
        });
        let terrain = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terrain"),
            layout: Some(&layout(&[&frame_layout])),
            vertex: wgpu::VertexState {
                module: &dm,
                entry_point: Some("terrain_vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: prim(None),
            depth_stencil: Some(depth(wgpu::CompareFunction::Less, Default::default())),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &dm,
                entry_point: Some("terrain_fs"),
                compilation_options: Default::default(),
                targets: &[target(srgb)],
            }),
            multiview_mask: None,
            cache: None,
        });
        let blit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&layout(&[&blit_layout])),
            vertex: wgpu::VertexState {
                module: &bm,
                entry_point: Some("blit_vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &bm,
                entry_point: Some("blit_fs"),
                compilation_options: Default::default(),
                targets: &[target(wgpu::TextureFormat::Rgba8Unorm)],
            }),
            multiview_mask: None,
            cache: None,
        });
        Spike {
            device,
            queue,
            cull_layout,
            place_layout,
            frame_layout,
            shadow_frame_layout,
            grazer_layout,
            blit_layout,
            cull,
            place,
            emit,
            shadow,
            shade,
            terrain,
            blit,
            frame: None,
            _lock: lock,
        }
    }

    fn buffer(&self, size: u64, usage: wgpu::BufferUsages, data: Option<&[u8]>) -> wgpu::Buffer {
        let size = size.max(16).div_ceil(16) * 16;
        match data {
            Some(bytes) => {
                let mut padded = bytes.to_vec();
                padded.resize(size as usize, 0);
                self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: &padded,
                    usage,
                })
            }
            None => self.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size,
                usage,
                mapped_at_creation: false,
            }),
        }
    }

    fn scratch(&self, max_blocks: u32) -> Scratch {
        use wgpu::BufferUsages as U;
        let b = u64::from(max_blocks);
        let words = |w: &[u32]| w.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        Scratch {
            live: self.buffer(b * 8, U::STORAGE | U::COPY_SRC, None),
            live_args: self.buffer(12, U::STORAGE | U::INDIRECT | U::COPY_DST | U::COPY_SRC, None),
            live_init: self.buffer(12, U::COPY_SRC, Some(&words(&[0, 1, 1]))),
            block_map: self.buffer(b * 4, U::STORAGE | U::COPY_DST, None),
            cells: self.buffer(b * 64 * 4, U::STORAGE, None),
            draw_init: self.buffer(32, U::COPY_SRC, Some(&words(&[0, 1, 0, 0, 0, 0, 0, 0]))),
        }
    }

    fn individual(&self, seed: usize, cell: f64, vcap: u32, icap: u32, ex: &Scratch) -> Individual {
        use wgpu::BufferUsages as U;
        let s = &data().seeds[seed];
        let grid = grid_for(s, cell);
        let mut raw = Vec::with_capacity(48);
        for o in grid.origin {
            raw.extend((o as f32).to_le_bytes());
        }
        raw.extend((cell as f32).to_le_bytes());
        for d in grid.dims {
            raw.extend(d.to_le_bytes());
        }
        for w in [grid.blocks, vcap, icap, ITERS] {
            raw.extend(w.to_le_bytes());
        }
        raw.extend(FALLOFF.to_le_bytes());
        let params: Vec<u8> = s.params.iter().flat_map(|x| x.to_le_bytes()).collect();
        let params = self.buffer(params.len() as u64, U::UNIFORM, Some(&params));
        let grid_buf = self.buffer(48, U::UNIFORM, Some(&raw));
        let verts = self.buffer(u64::from(vcap) * 48, U::STORAGE | U::VERTEX | U::COPY_SRC, None);
        let indices = self.buffer(u64::from(icap) * 4, U::STORAGE | U::INDEX | U::COPY_SRC, None);
        let draw = self.buffer(32, U::STORAGE | U::INDIRECT | U::COPY_DST | U::COPY_SRC, None);
        let all = [
            &params,
            &grid_buf,
            &ex.live,
            &ex.live_args,
            &ex.block_map,
            &ex.cells,
            &verts,
            &draw,
            &indices,
        ];
        let group = |layout: &wgpu::BindGroupLayout, skip: u32| {
            let entries: Vec<wgpu::BindGroupEntry> = all
                .iter()
                .enumerate()
                .filter(|(b, _)| *b as u32 != skip)
                .map(|(b, buf)| wgpu::BindGroupEntry {
                    binding: b as u32,
                    resource: buf.as_entire_binding(),
                })
                .collect();
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout,
                entries: &entries,
            })
        };
        let cull_bg = group(&self.cull_layout, 99);
        let place_bg = group(&self.place_layout, 3);
        Individual { seed, grid, vcap, icap, params, verts, indices, draw, cull_bg, place_bg }
    }

    /// main.js's `encodeExtract`: three passes, each timed at `q0..q0 + 6` if `qs` is given.
    fn encode_extract(
        &self,
        enc: &mut wgpu::CommandEncoder,
        ex: &Scratch,
        ind: &Individual,
        qs: Option<(&wgpu::QuerySet, u32)>,
    ) {
        enc.clear_buffer(&ex.block_map, 0, Some(u64::from(ind.grid.blocks) * 4));
        enc.copy_buffer_to_buffer(&ex.live_init, 0, &ex.live_args, 0, 12);
        enc.copy_buffer_to_buffer(&ex.draw_init, 0, &ind.draw, 0, 32);
        let writes = |i: u32| {
            qs.map(|(q, q0)| wgpu::ComputePassTimestampWrites {
                query_set: q,
                beginning_of_pass_write_index: Some(q0 + 2 * i),
                end_of_pass_write_index: Some(q0 + 2 * i + 1),
            })
        };
        {
            let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("cull_blocks"),
                timestamp_writes: writes(0),
            });
            p.set_pipeline(&self.cull);
            p.set_bind_group(0, &ind.cull_bg, &[]);
            p.dispatch_workgroups(ind.grid.blocks.div_ceil(64), 1, 1);
        }
        for (i, pipe) in [(1, &self.place), (2, &self.emit)] {
            let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: writes(i),
            });
            p.set_pipeline(pipe);
            p.set_bind_group(0, &ind.place_bg, &[]);
            p.dispatch_workgroups_indirect(&ex.live_args, 0);
        }
    }

    /// main.js's `extractHerd`: seeds 1 to `count` at `cell`, sized by a calibration on the
    /// largest grid, extracted three times; the last run's counts and times.
    pub fn extract_herd(&self, cell: f64, count: usize) -> (Vec<Extracted>, Vec<Individual>) {
        let seeds = &data().seeds[..count];
        let grids: Vec<Grid> = seeds.iter().map(|s| grid_for(s, cell)).collect();
        let max_blocks = grids.iter().map(|g| g.blocks).max().expect("a grid");
        let ex = self.scratch(max_blocks);
        let largest = grids.iter().position(|g| g.blocks == max_blocks).expect("the largest");
        let cal_cap = (max_blocks * 64).div_ceil(8).min(2_000_000);
        let cal = self.individual(largest, cell, cal_cap, cal_cap * 6, &ex);
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.encode_extract(&mut enc, &ex, &cal, None);
        self.queue.submit([enc.finish()]);
        let c = u32s(&read(&self.device, &self.queue, &cal.draw, 32));
        let vcap = (f64::from(c[5]) * 1.25).ceil() as u32 + 1024;
        let icap = (f64::from(c[0]) * 1.25).ceil() as u32 + 6144;
        let inds: Vec<Individual> =
            (0..count).map(|i| self.individual(i, cell, vcap, icap, &ex)).collect();
        let n = (count * 6) as u32;
        let mut out = Vec::new();
        for _ in 0..3 {
            let qs = self.device.create_query_set(&wgpu::QuerySetDescriptor {
                label: None,
                ty: wgpu::QueryType::Timestamp,
                count: n,
            });
            let stats = self.buffer(
                count as u64 * 16,
                wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                None,
            );
            let mut enc = self.device.create_command_encoder(&Default::default());
            for (i, ind) in inds.iter().enumerate() {
                self.encode_extract(&mut enc, &ex, ind, Some((&qs, (i * 6) as u32)));
                enc.copy_buffer_to_buffer(&ex.live_args, 0, &stats, i as u64 * 16, 12);
            }
            self.queue.submit([enc.finish()]);
            let ts = stamps(&self.device, &self.queue, &qs, n);
            let stats = u32s(&read(&self.device, &self.queue, &stats, count as u64 * 16));
            let ms = |i: usize, k: usize| (ts[i * 6 + 2 * k + 1] - ts[i * 6 + 2 * k]) / 1e6;
            out = inds
                .iter()
                .enumerate()
                .map(|(i, ind)| {
                    let d = u32s(&read(&self.device, &self.queue, &ind.draw, 32));
                    Extracted {
                        blocks: ind.grid.blocks,
                        live: stats[i * 4],
                        verts: d[5],
                        tris: d[0] / 3,
                        holes: d[6],
                        flags: d[7],
                        cull_ms: ms(i, 0),
                        place_ms: ms(i, 1),
                        emit_ms: ms(i, 2),
                    }
                })
                .collect();
        }
        (out, inds)
    }

    /// Seed `seed`'s live blocks at `cell`, from a fresh extraction: each block and its mask.
    pub fn live_masks(&self, seed: usize, cell: f64) -> Vec<(u32, u32)> {
        let grid = grid_for(&data().seeds[seed], cell);
        let ex = self.scratch(grid.blocks);
        let ind = self.individual(seed, cell, grid.blocks * 8, grid.blocks * 48, &ex);
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.encode_extract(&mut enc, &ex, &ind, None);
        self.queue.submit([enc.finish()]);
        let n = u32s(&read(&self.device, &self.queue, &ex.live_args, 16))[0];
        let live = u32s(&read(&self.device, &self.queue, &ex.live, u64::from(n.max(2)) * 8));
        live.chunks_exact(2).take(n as usize).map(|c| (c[0], c[1])).collect()
    }

    /// An individual's mesh: its vertices' rest positions and its triangles' indices.
    pub fn mesh(&self, ind: &Individual) -> (Vec<[f32; 3]>, Vec<u32>) {
        let d = u32s(&read(&self.device, &self.queue, &ind.draw, 32));
        let (nv, ni) = (u64::from(d[5].min(ind.vcap)), u64::from(d[0].min(ind.icap)));
        let v = read(&self.device, &self.queue, &ind.verts, (nv * 48).max(16));
        let positions = v
            .chunks_exact(48)
            .take(nv as usize)
            .map(|c| {
                let f = |k: usize| f32::from_le_bytes(c[4 * k..4 * k + 4].try_into().expect("4"));
                [f(0), f(1), f(2)]
            })
            .collect();
        let i = read(&self.device, &self.queue, &ind.indices, (ni * 4).max(16));
        (positions, u32s(&i)[..ni as usize].to_vec())
    }

    /// An individual's vertices as the spike wrote them: 48 bytes each (position, four bones a
    /// byte each, normal, four weights a byte each, the part mask, padding).
    pub fn vertices(&self, ind: &Individual) -> Vec<[u32; 12]> {
        let d = u32s(&read(&self.device, &self.queue, &ind.draw, 32));
        let nv = u64::from(d[5].min(ind.vcap));
        let v = read(&self.device, &self.queue, &ind.verts, (nv * 48).max(16));
        v.chunks_exact(48)
            .take(nv as usize)
            .map(|c| {
                std::array::from_fn(|k| {
                    u32::from_le_bytes(c[4 * k..4 * k + 4].try_into().expect("4"))
                })
            })
            .collect()
    }

    fn frame_resources(&mut self) {
        if self.frame.is_some() {
            return;
        }
        use wgpu::{BufferUsages as U, TextureUsages as T};
        let tex = |w, h, format, usage| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let color = tex(
            W,
            H,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            T::RENDER_ATTACHMENT | T::TEXTURE_BINDING,
        )
        .create_view(&Default::default());
        let depth = tex(W, H, wgpu::TextureFormat::Depth32Float, T::RENDER_ATTACHMENT)
            .create_view(&Default::default());
        let shadow = tex(
            2048,
            2048,
            wgpu::TextureFormat::Depth32Float,
            T::RENDER_ATTACHMENT | T::TEXTURE_BINDING,
        )
        .create_view(&Default::default());
        let canvas = tex(W, H, wgpu::TextureFormat::Rgba8Unorm, T::RENDER_ATTACHMENT | T::COPY_SRC);
        // Only the lookup mode samples the detail texture: field shading binds it unread.
        let detail = self
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("detail"),
                size: wgpu::Extent3d { width: 4, height: 4, depth_or_array_layers: 4 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D3,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: T::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let view = self.buffer(160, U::UNIFORM | U::COPY_DST, None);
        let palette = self.buffer(HERD as u64 * PALETTE_STRIDE, U::STORAGE | U::COPY_DST, None);
        let shadow_sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            compare: Some(wgpu::CompareFunction::Less),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let linear = self.device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let frame_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.frame_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: view.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&shadow),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&shadow_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&detail),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&linear),
                },
            ],
        });
        let shadow_frame_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.shadow_frame_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: view.as_entire_binding() }],
        });
        let blit_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&color),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&linear),
                },
            ],
        });
        self.frame = Some(FrameResources {
            color,
            depth,
            shadow,
            canvas,
            view,
            palette,
            frame_bg,
            shadow_frame_bg,
            blit_bg,
        });
    }

    /// main.js's `scene`: these individuals drawn, in slot order.
    pub fn scene<'a>(&mut self, kind: SceneKind, inds: Vec<&'a Individual>) -> Scene<'a> {
        self.frame_resources();
        let fr = self.frame.as_ref().expect("frame resources");
        let draws = inds
            .iter()
            .enumerate()
            .map(|(slot, ind)| {
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &self.grazer_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: ind.params.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &fr.palette,
                                offset: slot as u64 * PALETTE_STRIDE,
                                size: wgpu::BufferSize::new((BONES * 64) as u64),
                            }),
                        },
                    ],
                })
            })
            .collect();
        let tris = inds
            .iter()
            .map(|i| u64::from(u32s(&read(&self.device, &self.queue, &i.draw, 32))[0] / 3))
            .sum();
        Scene { kind, inds, draws, tris }
    }

    /// main.js's `update`: the view at time `t`, and the palettes `pal` (one time's, from
    /// [`palettes`]).
    pub fn update(&self, scene: &Scene, t: f64, pal: &[f32]) {
        let fr = self.frame.as_ref().expect("frame resources");
        let d = data();
        let mut v = match scene.kind {
            SceneKind::Herd => d.herd_view.clone(),
            SceneKind::CloseUp => d.closeup_view.clone(),
        };
        v[35] = t as f32;
        let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
        self.queue.write_buffer(&fr.view, 0, &bytes);
        let mut out = vec![0u8; scene.inds.len() * PALETTE_STRIDE as usize];
        for (i, m) in pal.chunks_exact(BONES * 16).enumerate().take(scene.inds.len()) {
            let at = i * PALETTE_STRIDE as usize;
            for (k, x) in m.iter().enumerate() {
                out[at + 4 * k..at + 4 * k + 4].copy_from_slice(&x.to_le_bytes());
            }
        }
        self.queue.write_buffer(&fr.palette, 0, &out);
    }

    /// main.js's `encodeFrame` in field mode with no prepass: the shadow pass, the terrain and
    /// the creatures, timed at `q0..q0 + 6` (pass 0, 1 and 2) if `qs` is given.
    pub fn encode_frame(
        &self,
        enc: &mut wgpu::CommandEncoder,
        scene: &Scene,
        qs: Option<(&wgpu::QuerySet, u32)>,
    ) {
        let fr = self.frame.as_ref().expect("frame resources");
        let writes = |i: u32| {
            qs.map(|(q, q0)| wgpu::RenderPassTimestampWrites {
                query_set: q,
                beginning_of_pass_write_index: Some(q0 + 2 * i),
                end_of_pass_write_index: Some(q0 + 2 * i + 1),
            })
        };
        let draw_all = |p: &mut wgpu::RenderPass| {
            for (ind, bg) in scene.inds.iter().zip(&scene.draws) {
                p.set_bind_group(1, bg, &[]);
                p.set_vertex_buffer(0, ind.verts.slice(..));
                p.set_index_buffer(ind.indices.slice(..), wgpu::IndexFormat::Uint32);
                p.draw_indexed_indirect(&ind.draw, 0);
            }
        };
        let depth = |view, load| {
            Some(wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations { load, store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            })
        };
        {
            let mut p = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow"),
                color_attachments: &[],
                depth_stencil_attachment: depth(&fr.shadow, wgpu::LoadOp::Clear(1.0)),
                timestamp_writes: writes(0),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            p.set_pipeline(&self.shadow);
            p.set_bind_group(0, &fr.shadow_frame_bg, &[]);
            draw_all(&mut p);
        }
        {
            let mut p = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terrain"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &fr.color,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.42,
                            g: 0.55,
                            b: 0.75,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: depth(&fr.depth, wgpu::LoadOp::Clear(1.0)),
                timestamp_writes: writes(1),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            p.set_pipeline(&self.terrain);
            p.set_bind_group(0, &fr.frame_bg, &[]);
            p.draw(0..TERRAIN_VERTS, 0..1);
        }
        {
            let mut p = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shade"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &fr.color,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: depth(&fr.depth, wgpu::LoadOp::Load),
                timestamp_writes: writes(2),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            p.set_pipeline(&self.shade);
            p.set_bind_group(0, &fr.frame_bg, &[]);
            draw_all(&mut p);
        }
    }

    /// One frame at `t`, as the spike's canvas shows it: RGBA8, rows top to bottom.
    pub fn render(&self, scene: &Scene, t: f64) -> Vec<u8> {
        let pal = palettes(scene.kind, &[t]);
        self.update(scene, t, &pal);
        let fr = self.frame.as_ref().expect("frame resources");
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.encode_frame(&mut enc, scene, None);
        {
            let view = fr.canvas.create_view(&Default::default());
            let mut p = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            p.set_pipeline(&self.blit);
            p.set_bind_group(0, &fr.blit_bg, &[]);
            p.draw(0..3, 0..1);
        }
        let size = u64::from(W) * u64::from(H) * 4;
        let rb = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("canvas"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &fr.canvas,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &rb,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(W * 4),
                    rows_per_image: Some(H),
                },
            },
            wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        wrela_host::map_read(&self.device, &rb).expect("the canvas")
    }

    /// main.js's `measure`: `warm` untimed frames, then `frames` timed ones, back to back, at
    /// t = f / 60. Each frame's times.
    pub fn measure(&self, scene: &Scene, warm: u32, frames: u32) -> Vec<FrameTimes> {
        let times: Vec<f64> = (0..warm.max(frames)).map(|f| f64::from(f) / 60.0).collect();
        let pal = palettes(scene.kind, &times);
        let per = scene.inds.len() * BONES * 16;
        let frame = |f: usize| &pal[f * per..(f + 1) * per];
        for (f, &t) in times.iter().enumerate().take(warm as usize) {
            self.update(scene, t, frame(f));
            let mut enc = self.device.create_command_encoder(&Default::default());
            self.encode_frame(&mut enc, scene, None);
            self.queue.submit([enc.finish()]);
        }
        let q = 6;
        let qs = self.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: None,
            ty: wgpu::QueryType::Timestamp,
            count: frames * q,
        });
        for (f, &t) in times.iter().enumerate().take(frames as usize) {
            self.update(scene, t, frame(f));
            let mut enc = self.device.create_command_encoder(&Default::default());
            self.encode_frame(&mut enc, scene, Some((&qs, f as u32 * q)));
            self.queue.submit([enc.finish()]);
        }
        let ts = stamps(&self.device, &self.queue, &qs, frames * q);
        (0..frames as usize)
            .map(|f| {
                let t = &ts[f * q as usize..(f + 1) * q as usize];
                let d = |i: usize| (t[2 * i + 1] - t[2 * i]) / 1e6;
                FrameTimes {
                    shadow: d(0),
                    terrain: d(1),
                    shade: d(2),
                    creatures: d(0) + d(2),
                    frame: (t[5] - t[0]) / 1e6,
                    passes: d(0) + d(1) + d(2),
                }
            })
            .collect()
    }
}

impl Default for Spike {
    fn default() -> Spike {
        Spike::new()
    }
}

/// The mean of |a − b| over the colour channels of two RGBA8 images, in /255, and the share of
/// pixels with a channel more than 8/255 apart (#42's image comparison).
pub fn image_difference(a: &[u8], b: &[u8]) -> (f64, f64) {
    assert_eq!(a.len(), b.len(), "the images differ in size");
    let (mut sum, mut far, mut n) = (0u64, 0u64, 0u64);
    for (p, q) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let mut most = 0;
        for c in 0..3 {
            let d = u64::from(p[c].abs_diff(q[c]));
            sum += d;
            most = most.max(d);
        }
        far += u64::from(most > 8);
        n += 1;
    }
    (sum as f64 / (3 * n) as f64, far as f64 / n as f64)
}
