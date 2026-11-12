//! Why a pixel has its colour (`wrela pixel`, language.md §23.9): every draw of a frame that
//! touched it, the draw that decided it, that draw's fragment there, and the fragment shader
//! run again in the interpreter of GPU code (`wrela_ir::interp`) for that one pixel, with each
//! source line's values; and the same for one invocation of a kernel.
//!
//! The program runs from `init` with its input script, which gives the same commands every
//! time (§11.1), so a frame can be run again and be the same frame. The native host's probe
//! (`wrela_host::Probe`) stops the frame's GPU work before a chosen draw: run once for each
//! draw, it gives the pixel before and after each one, and stopped before the draw that's
//! traced, the GPU holds what that draw sees, which is read back as its inputs
//! (`crate::captured`).

use crate::GpuModule;
use crate::captured;
use std::collections::BTreeMap;
use std::path::Path;
use wrela_abi::input::Scripted;
use wrela_abi::stream::{Binding, Command};
use wrela_diag::{SourceMap, Span};
use wrela_host::{Host, Options, Probe};
use wrela_ir::interp::concrete::{Given, floats_of, fvec, uvec3};
use wrela_ir::interp::raster::{self, Viewport};
use wrela_ir::interp::{Interp, Mark, Observer, Outcome, V};
use wrela_ir::{BuiltinInput, FuncId, Stage, TypeId, ValueId};

/// What to look at.
#[derive(Clone, Debug)]
pub struct Query {
    pub frame: u32,
    pub size: (u32, u32),
    pub script: Vec<Scripted>,
    pub fps: f64,
    pub what: What,
}

/// A pixel of the screen, or one invocation of a kernel's last dispatch under a label.
#[derive(Clone, Debug)]
pub enum What {
    Pixel { at: [u32; 2], draw: Option<usize>, fragment: Option<usize> },
    Kernel { label: String, id: u32 },
}

/// One draw that changed the pixel: its place among the frame's draws (from 1), its pipeline,
/// the label in force, its counts, and the pixel before and after it (RGBA8).
#[derive(Clone, Debug)]
pub struct Drawn {
    pub draw: usize,
    pub pipeline: String,
    pub label: Option<String>,
    pub vertices: u32,
    pub instances: u32,
    pub before: [u8; 4],
    pub after: [u8; 4],
}

/// A fragment of the traced draw at the pixel: its triangle, its depth, and whether it passed.
#[derive(Clone, Debug)]
pub struct Fragment {
    pub triangle: usize,
    pub depth: f64,
    pub passed: bool,
}

/// A source line's values: how many times it ran, and its first, last, least and most values
/// (a vector's components).
#[derive(Clone, Debug)]
pub struct Line {
    pub span: Span,
    pub ran: u32,
    pub first: Vec<f32>,
    pub last: Vec<f32>,
    pub least: Vec<f32>,
    pub most: Vec<f32>,
}

/// A branch's counts, by the line it's on.
#[derive(Clone, Debug)]
pub struct Branch {
    pub span: Span,
    pub taken: u32,
    pub not_taken: u32,
}

/// What the run found.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub history: Vec<Drawn>,
    /// The traced draw (or dispatch), its fragments at the pixel, the one traced.
    pub traced: Option<usize>,
    pub fragments: Vec<Fragment>,
    pub fragment: Option<usize>,
    pub vertex: Option<String>,
    pub shader: Option<String>,
    pub varyings: Option<V>,
    pub depth: Option<f64>,
    /// The interpreted result, and the GPU's (the pixel after the draw, as floats).
    pub interpreted: Option<Vec<f32>>,
    pub gpu: Option<Vec<f32>>,
    pub first_not_finite: Option<Span>,
    pub undefined: Vec<Mark>,
    pub approximate: Vec<Mark>,
    pub lines: Vec<Line>,
    pub branches: Vec<Branch>,
    /// Every value in order: its line, and its components.
    pub full: Vec<(Option<Span>, Vec<f32>)>,
    /// Why the shader couldn't be traced (the interpreter's `Unsupported`), with the history.
    pub unsupported: Option<String>,
    /// The derivative passes the block of four took.
    pub passes: u32,
}

fn host_err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// A host of `build` run to the end of frame `q.frame`, with `probe`, recording its batches:
/// the frame's batches.
fn run(build: &Path, q: &Query, probe: Option<Probe>) -> Result<Ran, String> {
    let mut host = Host::load_with(build, &Options { record: true, ..Options::default() })
        .map_err(host_err)?;
    host.set_probe(probe);
    let mut before = Vec::new();
    for i in 0..q.frame {
        host.lockstep_frame(i, q.fps, q.size.0, q.size.1, &q.script).map_err(host_err)?;
        before.extend(host.take_batches());
    }
    host.lockstep_frame(q.frame, q.fps, q.size.0, q.size.1, &q.script).map_err(host_err)?;
    let batches = host.take_batches();
    Ok((host, before, batches))
}

/// A host run to the end of the frame, the batches before it, and the frame's own.
type Ran = (Host, Vec<Vec<u8>>, Vec<Vec<u8>>);

/// A traced pixel's interpreted colour (none where the draw has no fragment there), and the
/// GPU's pixel after the draw (RGBA8).
pub type Traced = (Option<Vec<f32>>, [u8; 4]);

/// A draw of the frame, decoded.
#[derive(Clone, Debug)]
struct DrawCmd {
    pipeline: u32,
    vertices: u32,
    instances: u32,
    bindings: Vec<Binding>,
    uniforms: Vec<u8>,
    screen: bool,
    label: Option<String>,
    /// An indexed draw's index buffer: not traced yet.
    indexed: Option<Binding>,
}

/// A dispatch of the frame, decoded.
#[derive(Clone, Debug)]
struct DispatchCmd {
    pipeline: u32,
    groups: [u32; 3],
    bindings: Vec<Binding>,
    uniforms: Vec<u8>,
    label: Option<String>,
    /// How many draws came before it in the frame.
    draws_before: u32,
}

fn decode(batches: &[Vec<u8>]) -> (Vec<DrawCmd>, Vec<DispatchCmd>) {
    let (mut draws, mut dispatches) = (Vec::new(), Vec::new());
    let (mut screen, mut label) = (false, None);
    for batch in batches {
        for cmd in wrela_abi::stream::decode(batch).unwrap_or_default() {
            match cmd {
                Command::BeginScreenPass { .. } => screen = true,
                Command::BeginPass(_) => screen = false,
                Command::Label { name } => label = Some(name.to_string()),
                Command::Draw { pipeline, vertices, instances, bindings, uniforms } => {
                    draws.push(DrawCmd {
                        pipeline,
                        vertices,
                        instances,
                        bindings,
                        uniforms: uniforms.to_vec(),
                        screen,
                        label: label.clone(),
                        indexed: None,
                    });
                }
                Command::DrawIndexed {
                    pipeline,
                    indices,
                    index_offset,
                    index_size,
                    index_count,
                    instances,
                    first_index: _,
                    base_vertex: _,
                    first_instance: _,
                    bindings,
                    uniforms,
                } => draws.push(DrawCmd {
                    pipeline,
                    vertices: index_count,
                    instances,
                    bindings,
                    uniforms: uniforms.to_vec(),
                    screen,
                    label: label.clone(),
                    indexed: Some(Binding::range(indices, index_offset, index_size)),
                }),
                Command::DrawIndirect { .. } | Command::DrawIndexedIndirect { .. } => {
                    draws.push(DrawCmd {
                        pipeline: u32::MAX,
                        vertices: 0,
                        instances: 0,
                        bindings: Vec::new(),
                        uniforms: Vec::new(),
                        screen,
                        label: label.clone(),
                        indexed: None,
                    });
                }
                Command::Dispatch { pipeline, groups, bindings, uniforms } => {
                    dispatches.push(DispatchCmd {
                        pipeline,
                        groups,
                        bindings,
                        uniforms: uniforms.to_vec(),
                        label: label.clone(),
                        draws_before: draws.len() as u32,
                    });
                }
                _ => {}
            }
        }
    }
    (draws, dispatches)
}

/// The screen's pixel at `at`, RGBA8.
fn pixel_of(host: &mut Host, at: [u32; 2]) -> Result<[u8; 4], String> {
    let px = host.read_screen_region(at[0], at[1], 1, 1).map_err(host_err)?;
    Ok([px[0], px[1], px[2], px[3]])
}

/// What the line-by-line walk records.
#[derive(Default)]
struct Lines {
    at: Option<Span>,
    lines: BTreeMap<(u32, u32), Line>,
    branches: BTreeMap<(u32, u32), Branch>,
    full: Vec<(Option<Span>, Vec<f32>)>,
    first_not_finite: Option<Span>,
}

impl Observer for Lines {
    fn at(&mut self, span: Span) {
        self.at = Some(span);
    }
    fn value(&mut self, _f: FuncId, _v: ValueId, _ty: TypeId, val: &mut V) {
        let xs = floats_of(val);
        if xs.is_empty() {
            return;
        }
        if self.first_not_finite.is_none() && xs.iter().any(|x| !x.is_finite()) {
            self.first_not_finite = self.at;
        }
        self.full.push((self.at, xs.clone()));
        let Some(span) = self.at else { return };
        let line = self.lines.entry((span.file.0, span.start)).or_insert_with(|| Line {
            span,
            ran: 0,
            first: xs.clone(),
            last: xs.clone(),
            least: xs.clone(),
            most: xs.clone(),
        });
        line.ran += 1;
        line.last = xs.clone();
        if xs.len() == line.least.len() {
            for (i, x) in xs.iter().enumerate() {
                line.least[i] = line.least[i].min(*x);
                line.most[i] = line.most[i].max(*x);
            }
        }
    }
    fn branch(&mut self, taken: bool) {
        let Some(span) = self.at else { return };
        let b = self.branches.entry((span.file.0, span.start)).or_insert(Branch {
            span,
            taken: 0,
            not_taken: 0,
        });
        if taken {
            b.taken += 1;
        } else {
            b.not_taken += 1;
        }
    }
}

/// Runs the query on the build at `build`, whose pipelines are `gpu` (from
/// [`crate::gpu_modules`]).
pub fn query(build: &Path, gpu: &[GpuModule], q: &Query) -> Result<Report, String> {
    match &q.what {
        What::Pixel { at, draw, fragment } => pixel(build, gpu, q, *at, *draw, *fragment),
        What::Kernel { label, id } => kernel(build, gpu, q, label, *id),
    }
}

fn pixel(
    build: &Path,
    gpu: &[GpuModule],
    q: &Query,
    at: [u32; 2],
    choice: Option<usize>,
    fragment: Option<usize>,
) -> Result<Report, String> {
    let (_, before_batches, batches) = run(build, q, Some(Probe { frame: q.frame, draws: 0 }))?;
    let (draws, _) = decode(&batches);
    let mut report = Report::default();
    // The pixel after each draw: the frame stopped after it.
    let mut last = {
        let (mut h, _, _) = run(build, q, Some(Probe { frame: q.frame, draws: 0 }))?;
        pixel_of(&mut h, at)?
    };
    for (k, d) in draws.iter().enumerate() {
        if !d.screen {
            continue;
        }
        let (mut h, _, _) = run(build, q, Some(Probe { frame: q.frame, draws: k as u32 + 1 }))?;
        let after = pixel_of(&mut h, at)?;
        let name = gpu.get(d.pipeline as usize).map_or_else(|| "?".into(), |g| g.name.clone());
        // The draws that changed the pixel: one that wrote what was there is left out.
        if after == last {
            continue;
        }
        report.history.push(Drawn {
            draw: k + 1,
            pipeline: name,
            label: d.label.clone(),
            vertices: d.vertices,
            instances: d.instances,
            before: last,
            after,
        });
        last = after;
    }
    let chosen = match choice {
        Some(k) => Some(k),
        None => report.history.iter().rev().find(|h| h.after != h.before).map(|h| h.draw),
    };
    let Some(k) = chosen else { return Ok(report) };
    report.traced = Some(k);
    let d = draws.get(k - 1).ok_or_else(|| format!("the frame has no draw {k}"))?;
    if d.pipeline == u32::MAX {
        report.unsupported = Some("an indirect draw".into());
        return Ok(report);
    }
    let g = gpu.get(d.pipeline as usize).ok_or("a draw of a pipeline the build doesn't have")?;
    let manifest = g.manifest.as_ref().ok_or("the pipeline has no manifest entry")?;
    report.gpu = report
        .history
        .iter()
        .find(|h| h.draw == k)
        .map(|h| h.after.map(|c| f32::from(c) / 255.0).to_vec());
    // What the draw sees: the frame stopped just before it.
    let (mut host, _, _) = run(build, q, Some(Probe { frame: q.frame, draws: k as u32 - 1 }))?;
    let all: Vec<Vec<u8>> = before_batches.iter().chain(&batches).cloned().collect();
    let samplers = captured::samplers(&all);
    let given = captured::given(manifest, &d.bindings, &d.uniforms, &samplers, &mut host)?;
    let drawing = Drawing::new(g, d, &given)?;
    report.vertex = Some(drawing.vertex.clone());
    report.shader = Some(drawing.shader.clone());
    if d.indexed.is_some() {
        report.unsupported = Some("an indexed draw".into());
        return Ok(report);
    }
    trace_pixel(&drawing, &given, Viewport::whole(q.size.0, q.size.1), at, fragment, &mut report)?;
    Ok(report)
}

/// A draw's pipeline, its entry points, and its vertex shader run for each of its vertices.
struct Drawing<'a> {
    m: &'a wrela_ir::Module,
    fe: usize,
    vertex: String,
    shader: String,
    varyings: Vec<wrela_ir::Varying>,
    fvaryings: Vec<wrela_ir::Varying>,
    results: Vec<V>,
}

impl<'a> Drawing<'a> {
    fn new(g: &'a GpuModule, d: &DrawCmd, given: &Given) -> Result<Drawing<'a>, String> {
        let m = &g.module;
        let ve = m.entry_points.iter().position(|e| matches!(e.stage, Stage::Vertex { .. }));
        let fe = m.entry_points.iter().position(|e| matches!(e.stage, Stage::Fragment { .. }));
        let (Some(ve), Some(fe)) = (ve, fe) else {
            return Err("the draw's pipeline isn't a render pipeline".into());
        };
        let Stage::Vertex { varyings } = &m.entry_points[ve].stage else { unreachable!() };
        let fvaryings = match &m.entry_points[fe].stage {
            Stage::Fragment { varyings: Some((_, v)), .. } => v.clone(),
            _ => varyings.clone(),
        };
        let mut results = Vec::new();
        if d.indexed.is_none() {
            for inst in 0..d.instances.max(1) {
                for i in 0..d.vertices {
                    let mut gv = given.clone();
                    gv.builtins = vec![
                        (BuiltinInput::VertexIndex, V::S(wrela_ir::Const::U32(i))),
                        (BuiltinInput::InstanceIndex, V::S(wrela_ir::Const::U32(inst))),
                    ];
                    let out = Interp::new(m, &mut gv, &mut ())
                        .run_entry(ve)
                        .map_err(|e| e.to_string())?;
                    results.push(out.result.ok_or("the vertex shader returned nothing")?);
                }
            }
        }
        Ok(Drawing {
            m,
            fe,
            vertex: g.entries.get(ve).cloned().unwrap_or_default(),
            shader: g.entries.get(fe).cloned().unwrap_or_default(),
            varyings: varyings.clone(),
            fvaryings,
            results,
        })
    }
}

/// The draw's fragments at pixel `at`, and the one traced (the last, or `fragment`), run in
/// its block of four and line by line, into `report`.
fn trace_pixel(
    dr: &Drawing<'_>,
    given: &Given,
    vp: Viewport,
    at: [u32; 2],
    fragment: Option<usize>,
    report: &mut Report,
) -> Result<(), String> {
    let (m, fe) = (dr.m, dr.fe);
    let centre = [at[0] as f64 + 0.5, at[1] as f64 + 0.5];
    report.fragments.clear();
    for (t, tri) in dr.results.chunks_exact(3).enumerate() {
        let clips: Option<Vec<[f32; 4]>> =
            tri.iter().map(|r| raster::clip_of(r, &dr.varyings)).collect();
        let Some(clips) = clips else { continue };
        let clip = [clips[0], clips[1], clips[2]];
        if let Some(h) = raster::hit(clip, centre[0], centre[1], &vp)
            && h.inside
        {
            // No depth target is replayed: each fragment passes.
            report.fragments.push(Fragment { triangle: t, depth: h.depth, passed: true });
        }
    }
    let pick = match fragment {
        Some(j) => (j < report.fragments.len()).then_some(j),
        None => report.fragments.iter().rposition(|f| f.passed),
    };
    report.fragment = pick;
    let Some(j) = pick else { return Ok(()) };
    let t = report.fragments[j].triangle;
    let tri = &dr.results[3 * t..3 * t + 3];
    let clip = [0, 1, 2].map(|i| raster::clip_of(&tri[i], &dr.varyings).expect("a clip position"));
    // The block of four: each of its pixels on the triangle's plane (a pixel outside it runs
    // as a helper invocation does), run in passes until the derivatives settle.
    let corner = [at[0] & !1, at[1] & !1];
    let block: Vec<[u32; 2]> =
        (0..4).map(|i| [corner[0] + (i & 1), corner[1] + (i >> 1)]).collect();
    let me = block.iter().position(|p| *p == at).expect("the pixel is in its block");
    let mut inputs: Vec<Given> = Vec::new();
    for p in &block {
        let c = [p[0] as f64 + 0.5, p[1] as f64 + 0.5];
        let h = raster::hit(clip, c[0], c[1], &vp).ok_or("the triangle is seen edge on")?;
        let mut gv = given.clone();
        let pos = fvec(&[c[0] as f32, c[1] as f32, h.depth as f32, h.inv_w as f32]);
        gv.builtins = vec![(BuiltinInput::Position, pos)];
        let corners = [&tri[0], &tri[1], &tri[2]];
        gv.varyings =
            Some(raster::interpolate(corners, &dr.fvaryings, &h, [c[0] as f32, c[1] as f32]));
        if p == &at {
            report.varyings = gv.varyings.clone();
            report.depth = Some(h.depth);
        }
        inputs.push(gv);
    }
    let mut passes = 0;
    let mut previous: Option<Vec<Vec<V>>> = None;
    loop {
        let mut operands = Vec::new();
        for gv in inputs.iter_mut() {
            gv.operands.clear();
            gv.ops.clear();
            Interp::new(m, gv, &mut ()).run_entry(fe).map_err(|e| e.to_string())?;
            operands.push(gv.operands.clone());
        }
        passes += 1;
        if operands.iter().all(Vec::is_empty) || previous.as_ref() == Some(&operands) {
            break;
        }
        if passes > 4 {
            return Err("derivatives nested more than three deep".into());
        }
        // Each pixel's answers: the differences across the block of the last pass's operands
        // (`dpdx`: its row's right less its left; `dpdy`: its column's lower less its upper;
        // `fwidth`: the sum of their sizes).
        let calls = operands.iter().map(Vec::len).min().unwrap_or(0);
        let ops = inputs[me].ops.clone();
        for (i, gv) in inputs.iter_mut().enumerate() {
            let (x, y) = (i & 1, i >> 1);
            gv.answers = (0..calls)
                .map(|j| {
                    let of = |k: usize| floats_of(&operands[k][j]);
                    let dx: Vec<f32> =
                        of(y * 2 + 1).iter().zip(of(y * 2)).map(|(r, l)| r - l).collect();
                    let dy: Vec<f32> = of(2 + x).iter().zip(of(x)).map(|(b, t)| b - t).collect();
                    let shape = &operands[i][j];
                    match ops.get(j) {
                        Some(wrela_ir::Builtin::Dpdy) => reshape(shape, &dy),
                        Some(wrela_ir::Builtin::Fwidth) => {
                            let w: Vec<f32> =
                                dx.iter().zip(&dy).map(|(a, b)| a.abs() + b.abs()).collect();
                            reshape(shape, &w)
                        }
                        _ => reshape(shape, &dx),
                    }
                })
                .collect();
        }
        previous = Some(operands);
    }
    report.passes = passes;
    // The traced pixel, line by line.
    let mut lines = Lines::default();
    let out: Outcome =
        Interp::new(m, &mut inputs[me], &mut lines).run_entry(fe).map_err(|e| e.to_string())?;
    report.interpreted = out.result.as_ref().map(|r| floats_of(r).into_iter().take(4).collect());
    report.undefined = out.undefined;
    report.approximate = out.approximate;
    report.first_not_finite = lines.first_not_finite;
    report.lines = lines.lines.into_values().collect();
    report.branches = lines.branches.into_values().collect();
    report.full = lines.full;
    Ok(())
}

/// Draw `draw` of frame `q.frame` (from 1), traced at each of `pixels` from one capture: for
/// each, the interpreted colour (none where the draw has no fragment there) and the GPU's
/// pixel after the draw (RGBA8). For measuring the interpreter against the GPU.
pub fn query_pixels(
    build: &Path,
    gpu: &[GpuModule],
    q: &Query,
    draw: usize,
    pixels: &[[u32; 2]],
) -> Result<Vec<Traced>, String> {
    let (_, before_batches, batches) = run(build, q, None)?;
    let (draws, _) = decode(&batches);
    let d =
        draws.get(draw.wrapping_sub(1)).ok_or_else(|| format!("the frame has no draw {draw}"))?;
    let g = gpu.get(d.pipeline as usize).ok_or("a draw of a pipeline the build doesn't have")?;
    let manifest = g.manifest.as_ref().ok_or("the pipeline has no manifest entry")?;
    let (mut after, _, _) = run(build, q, Some(Probe { frame: q.frame, draws: draw as u32 }))?;
    let (mut host, _, _) = run(build, q, Some(Probe { frame: q.frame, draws: draw as u32 - 1 }))?;
    let all: Vec<Vec<u8>> = before_batches.iter().chain(&batches).cloned().collect();
    let given =
        captured::given(manifest, &d.bindings, &d.uniforms, &captured::samplers(&all), &mut host)?;
    let drawing = Drawing::new(g, d, &given)?;
    let vp = Viewport::whole(q.size.0, q.size.1);
    let mut out = Vec::new();
    for &at in pixels {
        let mut r = Report::default();
        trace_pixel(&drawing, &given, vp, at, None, &mut r)?;
        out.push((r.interpreted, pixel_of(&mut after, at)?));
    }
    Ok(out)
}

/// `xs` in the shape of `like` (a scalar or a vector of `f32`s).
fn reshape(like: &V, xs: &[f32]) -> V {
    match like {
        V::P(_) => fvec(xs),
        V::S(_) => V::S(wrela_ir::Const::F32(xs.first().copied().unwrap_or(0.0))),
    }
}

fn kernel(
    build: &Path,
    gpu: &[GpuModule],
    q: &Query,
    label: &str,
    id: u32,
) -> Result<Report, String> {
    let (_, before_batches, batches) = run(build, q, None)?;
    let (_, dispatches) = decode(&batches);
    let d = dispatches
        .iter()
        .rev()
        .find(|d| {
            d.label.as_deref() == Some(label)
                || gpu
                    .get(d.pipeline as usize)
                    .is_some_and(|g| g.name == label || g.entries.iter().any(|e| e == label))
        })
        .ok_or_else(|| format!("frame {} dispatches nothing under `{label}`", q.frame))?;
    let g =
        gpu.get(d.pipeline as usize).ok_or("a dispatch of a pipeline the build doesn't have")?;
    let manifest = g.manifest.as_ref().ok_or("the pipeline has no manifest entry")?;
    // The GPU as the dispatch found it: stopped at the draw before it. A dispatch after the
    // frame's last draw sees what the whole frame did before it, which only stopping at it
    // would show: that is the frame run to its end, less the dispatches after it.
    let (mut host, _, _) = run(build, q, Some(Probe { frame: q.frame, draws: d.draws_before }))?;
    let all: Vec<Vec<u8>> = before_batches.iter().chain(&batches).cloned().collect();
    let mut given =
        captured::given(manifest, &d.bindings, &d.uniforms, &captured::samplers(&all), &mut host)?;
    let m = &g.module;
    let e = m
        .entry_points
        .iter()
        .position(|e| matches!(e.stage, Stage::Compute { .. }))
        .ok_or("not a kernel")?;
    let Stage::Compute { workgroup_size } = m.entry_points[e].stage else { unreachable!() };
    let wx = workgroup_size[0].max(1);
    given.builtins = vec![
        (BuiltinInput::GlobalInvocationId, uvec3(id, 0, 0)),
        (BuiltinInput::LocalInvocationId, uvec3(id % wx, 0, 0)),
        (BuiltinInput::WorkgroupId, uvec3(id / wx, 0, 0)),
        (BuiltinInput::NumWorkgroups, uvec3(d.groups[0], d.groups[1], d.groups[2])),
    ];
    let mut report = Report { shader: g.entries.get(e).cloned(), ..Report::default() };
    let mut lines = Lines::default();
    match Interp::new(m, &mut given, &mut lines).run_entry(e) {
        Ok(out) => {
            report.interpreted = out.writes.first().map(|w| floats_of(&w.value));
            report.undefined = out.undefined;
            report.approximate = out.approximate;
        }
        Err(wrela_ir::interp::Error::Unsupported(what)) => report.unsupported = Some(what),
        Err(e) => return Err(e.to_string()),
    }
    report.first_not_finite = lines.first_not_finite;
    report.lines = lines.lines.into_values().collect();
    report.branches = lines.branches.into_values().collect();
    report.full = lines.full;
    Ok(report)
}

/// A span's file, line and column, and the line's text.
pub fn place(sources: &SourceMap, span: Span) -> (String, u32, u32, String) {
    let file = sources.file(span.file);
    let lc = file.line_col(span.start);
    let line = file.line_index(span.start);
    (file.name.clone(), lc.line, lc.column, file.line_text(line).trim().to_string())
}
