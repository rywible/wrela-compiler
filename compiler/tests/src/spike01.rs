//! Spike 01's harness, ported to Rust (#42, "The harness is new code"): its kernels and its
//! `grazer.js`, unchanged, run the way its `main.js` runs them. `encodeExtract` and `extractHerd`
//! are ported here; the numbers that come from `grazer.js` (each seed's parameters and bounds)
//! come from it, run by bun (compiler/tests/spike01/data.mjs). The spike ran in Chrome; this runs
//! on the native host's wgpu, so the herd's own build can run beside it in one process.

use crate::{RawGpu, bytes_of, f32s, repo_root, u32s};
use std::path::PathBuf;
use std::sync::OnceLock;
use wgpu::util::DeviceExt;

/// Sketch 02's root iterations and the 6 cm skin falloff.
const ITERS: u32 = 4;
const FALLOFF: f32 = 0.06;

/// A file of the spike's (compiler/tests/fixtures/spike01).
pub fn fixture_path(name: &str) -> PathBuf {
    repo_root().join("compiler/tests/fixtures/spike01").join(name)
}

/// A text file of the spike's ([`fixture_path`]).
pub fn fixture(name: &str) -> String {
    std::fs::read_to_string(fixture_path(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Seed 1's parameters: the spike's `Grazer` uniform, 328 f32s.
pub fn params_seed1() -> Vec<f32> {
    f32s(&std::fs::read(fixture_path("params-seed1.f32")).expect("params-seed1.f32"))
}

/// A kernel to append to field.wgsl: each point's distance (`eval_d`), every part in. Its
/// bindings: the `Grazer` uniform, the points (vec4s) and the distances.
pub const HARNESS_D: &str = "
@group(0) @binding(0) var<uniform> G: Grazer;
@group(0) @binding(1) var<storage, read> points: array<vec4f>;
@group(0) @binding(2) var<storage, read_write> dist: array<f32>;

@compute @workgroup_size(64)
fn eval_d(@builtin(global_invocation_id) id: vec3u) {
  dist[id.x] = grazer_d(points[id.x].xyz, ALL_PARTS, 0.0);
}
";

/// A kernel `entry` to append to field.wgsl: `store` (a `vec4f` of `s`, the point's
/// `grazer_g` sample, every part in) for each point. Bound as [`HARNESS_D`] is, with vec4s out.
pub fn harness(entry: &str, store: &str) -> String {
    format!(
        "
@group(0) @binding(0) var<uniform> G: Grazer;
@group(0) @binding(1) var<storage, read> points: array<vec4f>;
@group(0) @binding(2) var<storage, read_write> samp: array<vec4f>;

@compute @workgroup_size(64)
fn {entry}(@builtin(global_invocation_id) id: vec3u) {{
  let s = grazer_g(points[id.x].xyz, ALL_PARTS, 0.0);
  samp[id.x] = {store};
}}
"
    )
}

/// One seed's numbers, as `makeGrazer` gives them.
struct Seed {
    /// The `Grazer` uniform: 328 floats.
    params: Vec<f32>,
    /// Its rest-space bounds, in f64 as JavaScript computed them.
    min: [f64; 3],
    max: [f64; 3],
}

fn bun<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Vec<u8> {
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

/// `grazer.js`'s seeds, once per process.
fn seeds() -> &'static [Seed] {
    static SEEDS: OnceLock<Vec<Seed>> = OnceLock::new();
    SEEDS.get_or_init(|| {
        let v: serde_json::Value = serde_json::from_slice(&bun(&["seeds"])).expect("JSON");
        let f = |x: &serde_json::Value| x.as_f64().expect("a number");
        let v3 = |x: &serde_json::Value| {
            let a = x.as_array().expect("a vector");
            [f(&a[0]), f(&a[1]), f(&a[2])]
        };
        let floats = |x: &serde_json::Value| {
            x.as_array().expect("floats").iter().map(|n| f(n) as f32).collect::<Vec<f32>>()
        };
        v["seeds"]
            .as_array()
            .expect("seeds")
            .iter()
            .map(|s| Seed { params: floats(&s["params"]), min: v3(&s["min"]), max: v3(&s["max"]) })
            .collect()
    })
}

/// A grid for one individual at one cell size (main.js's `gridFor`), in f64 as JavaScript.
#[derive(Clone, Copy, Debug)]
struct Grid {
    origin: [f64; 3],
    dims: [u32; 3],
    blocks: u32,
}

fn grid_for(s: &Seed, cell: f64) -> Grid {
    let pad = 2.0 * cell;
    let origin = [s.min[0] - pad, s.min[1] - pad, s.min[2] - pad];
    let dims = [0, 1, 2].map(|a| ((s.max[a] + pad - origin[a]) / (4.0 * cell)).ceil() as u32);
    Grid { origin, dims, blocks: dims[0] * dims[1] * dims[2] }
}

/// One individual's extraction: its counts, and its GPU time (its three passes'), ms.
#[derive(Clone, Copy, Debug, Default)]
pub struct Extracted {
    pub blocks: u32,
    pub live: u32,
    pub verts: u32,
    pub tris: u32,
    pub holes: u32,
    pub flags: u32,
    pub gpu_ms: f64,
}

/// An individual's buffers (main.js's `individual`).
pub struct Individual {
    grid: Grid,
    pub vcap: u32,
    pub icap: u32,
    verts: wgpu::Buffer,
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

/// The spike's extraction, on a device of its own: its pipelines.
pub struct Spike {
    cull_layout: wgpu::BindGroupLayout,
    place_layout: wgpu::BindGroupLayout,
    cull: wgpu::ComputePipeline,
    place: wgpu::ComputePipeline,
    emit: wgpu::ComputePipeline,
    // Declared last: dropped after what it made.
    gpu: RawGpu,
}

impl Spike {
    /// Opens a device (with timestamps) and builds the extraction's three pipelines (cull,
    /// place, emit).
    pub fn new() -> Spike {
        use wgpu::{BindGroupLayoutEntry as E, BindingType as B, ShaderStages as S};
        let gpu = RawGpu::new().expect("a GPU with timestamps");
        let device = gpu.device();
        let buf = |binding, ty| E {
            binding,
            visibility: S::COMPUTE,
            ty: B::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let storage = wgpu::BufferBindingType::Storage { read_only: false };
        let uniform = wgpu::BufferBindingType::Uniform;
        let extract_layout = |skip: u32| {
            let mut entries = vec![buf(0, uniform), buf(1, uniform)];
            entries.extend((2..9).filter(|b| *b != skip).map(|b| buf(b, storage)));
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &entries,
            })
        };
        let cull_layout = extract_layout(99);
        // live_args is the indirect buffer here, so it can't also be bound.
        let place_layout = extract_layout(3);
        let em = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("extract"),
            source: wgpu::ShaderSource::Wgsl(
                format!("{}{}", fixture("field.wgsl"), fixture("extract.wgsl")).into(),
            ),
        });
        let compute = |entry: &str, bgl: &wgpu::BindGroupLayout| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(bgl)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                module: &em,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let cull = compute("cull_blocks", &cull_layout);
        let place = compute("place_vertices", &place_layout);
        let emit = compute("emit_quads", &place_layout);
        Spike { cull_layout, place_layout, cull, place, emit, gpu }
    }

    fn buffer(&self, size: u64, usage: wgpu::BufferUsages, data: Option<&[u8]>) -> wgpu::Buffer {
        let size = size.max(16).div_ceil(16) * 16;
        match data {
            Some(bytes) => {
                let mut padded = bytes.to_vec();
                padded.resize(size as usize, 0);
                self.gpu.device().create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: &padded,
                    usage,
                })
            }
            None => self.gpu.device().create_buffer(&wgpu::BufferDescriptor {
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
        let s = &seeds()[seed];
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
        let params = bytes_of(&s.params);
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
            self.gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout,
                entries: &entries,
            })
        };
        let cull_bg = group(&self.cull_layout, 99);
        let place_bg = group(&self.place_layout, 3);
        Individual { grid, vcap, icap, verts, draw, cull_bg, place_bg }
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

    /// main.js's `extractHerd`: seeds 1 to `count` at `cell` by a calibration on the
    /// largest grid, extracted three times, each run's counts and times read back at once; the
    /// last run's.
    pub fn extract_herd(&self, cell: f64, count: usize) -> (Vec<Extracted>, Vec<Individual>) {
        use wgpu::BufferUsages as U;
        let seeds = &seeds()[..count];
        let grids: Vec<Grid> = seeds.iter().map(|s| grid_for(s, cell)).collect();
        let max_blocks = grids.iter().map(|g| g.blocks).max().expect("a grid");
        let ex = self.scratch(max_blocks);
        let largest = grids.iter().position(|g| g.blocks == max_blocks).expect("the largest");
        let cal_cap = (max_blocks * 64).div_ceil(8).min(2_000_000);
        let cal = self.individual(largest, cell, cal_cap, cal_cap * 6, &ex);
        let mut enc = self.gpu.device().create_command_encoder(&Default::default());
        self.encode_extract(&mut enc, &ex, &cal, None);
        self.gpu.queue().submit([enc.finish()]);
        let c = self.draw_args(&cal);
        let vcap = (f64::from(c[5]) * 1.25).ceil() as u32 + 1024;
        let icap = (f64::from(c[0]) * 1.25).ceil() as u32 + 6144;
        let inds: Vec<Individual> =
            (0..count).map(|i| self.individual(i, cell, vcap, icap, &ex)).collect();
        let passes = (count * 3) as u32;
        let (mut times, mut stats, mut draws) = (Vec::new(), Vec::new(), Vec::new());
        for _ in 0..3 {
            let qs = self.gpu.device().create_query_set(&wgpu::QuerySetDescriptor {
                label: None,
                ty: wgpu::QueryType::Timestamp,
                count: passes * 2,
            });
            // Each individual's live block count (`live_args`, which the next one's extraction
            // reuses) and draw arguments.
            let stats_buf = self.buffer(count as u64 * 16, U::COPY_DST | U::COPY_SRC, None);
            let draws_buf = self.buffer(count as u64 * 32, U::COPY_DST | U::COPY_SRC, None);
            let mut enc = self.gpu.device().create_command_encoder(&Default::default());
            for (i, ind) in inds.iter().enumerate() {
                self.encode_extract(&mut enc, &ex, ind, Some((&qs, (i * 6) as u32)));
                enc.copy_buffer_to_buffer(&ex.live_args, 0, &stats_buf, i as u64 * 16, 12);
                enc.copy_buffer_to_buffer(&ind.draw, 0, &draws_buf, i as u64 * 32, 32);
            }
            self.gpu.queue().submit([enc.finish()]);
            times = wrela_host::read_timestamps(self.gpu.device(), self.gpu.queue(), &qs, passes)
                .expect("the timestamps");
            stats = u32s(&self.gpu.read(&stats_buf, count as u64 * 16));
            draws = u32s(&self.gpu.read(&draws_buf, count as u64 * 32));
        }
        let out = inds
            .iter()
            .enumerate()
            .map(|(i, ind)| {
                let d = &draws[i * 8..i * 8 + 8];
                Extracted {
                    blocks: ind.grid.blocks,
                    live: stats[i * 4],
                    verts: d[5],
                    tris: d[0] / 3,
                    holes: d[6],
                    flags: d[7],
                    gpu_ms: times[i * 3..i * 3 + 3].iter().sum::<f64>() / 1e6,
                }
            })
            .collect();
        (out, inds)
    }

    /// An individual's draw arguments (main.js's): its index count first, its vertex count
    /// sixth.
    fn draw_args(&self, ind: &Individual) -> Vec<u32> {
        u32s(&self.gpu.read(&ind.draw, 32))
    }

    /// An individual's vertices as the spike wrote them: 48 bytes each (position, four bones a
    /// byte each, normal, four weights a byte each, the part mask, padding).
    pub fn vertices(&self, ind: &Individual) -> Vec<[u32; 12]> {
        let nv = self.draw_args(ind)[5].min(ind.vcap) as usize;
        let words = u32s(&self.gpu.read(&ind.verts, (nv as u64 * 48).max(16)));
        words.chunks_exact(12).take(nv).map(|v| v.try_into().expect("12 words")).collect()
    }
}

impl Default for Spike {
    fn default() -> Spike {
        Spike::new()
    }
}
