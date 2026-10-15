//! The engine's landforms on the GPU (`engine::landform::Relief`): a relief a terrain holds is
//! read by the same code on the CPU and the GPU, so the drawn ground and the sim's feet agree.

/// A full-size relief (the Last Green's far land's 40,960 cells, a storage block well over the
/// uniform limit, as the terrain's cook binds it) sampled by a kernel at its cells' middles, inside
/// cells, on and past its edges and at its last cell: the rise within 1 mm of the CPU's, each
/// cell's packed rise decoded exactly, its wetness within a float's rounding.
#[test]
fn a_relief_reads_the_same_on_the_gpu_as_on_the_cpu() {
    let dir = super::engine_package(
        "landform/relief-gpu",
        "relief_gpu",
        r#"use engine::grow::Grid
use engine::landform::{Relief, pack}
use std::gpu::{GlobalId, GpuBuffer, Slots, buffer, dispatch, read, write}
use std::io::Pending

const N: u32 = 40960
const GRID = Grid { lo: vec2(-8000.0, -4000.0), nx: 160, nz: 256, cell: 64.0 }

/// A terrain-like value holding a relief, passed whole to a kernel as the terrain is.
pub struct Held: Copy + GpuData {
    pub relief: Relief<N>,
}

/// Each point's rise and wetness, and the rise and wetness of the cell it's in.
@compute(64)
fn sample(held: Held, points: [vec2], out: mut Slots<vec4>, id: GlobalId) {
    let p = points[id.x]
    let c = GRID.cell_at(p)
    out[id] = vec4(
        held.relief.rise_at(p),
        held.relief.wet_at(p),
        held.relief.rise(c),
        held.relief.wet(c),
    )
}

fn held() -> Held {
    var r: Relief<N> = Relief::none(GRID)
    for c in 0..N {
        let x = f32(c)
        r.cells[c] = pack(rise: 300.0 * sin(x * 0.37) + 0.01 * x - 200.0, wet: fract(x * 0.618))
    }
    Held { relief: r }
}

fn points() -> Vec<vec2> {
    var out: Vec<vec2> = Vec::new()
    let hi = GRID.lo + vec2(f32(GRID.nx), f32(GRID.nz)) * GRID.cell
    for k in 0..512 {
        let t = f32(k) / 511.0
        out.push(GRID.centre(k * 80))
        out.push(mix(GRID.lo, hi, vec2(t, fract(t * 7.3))))
        out.push(vec2(mix(GRID.lo.x, hi.x, t), GRID.lo.y - 100.0))
        out.push(vec2(hi.x + 50.0, mix(GRID.lo.y, hi.y, t)))
    }
    out.push(GRID.centre(N - 1))
    out.push(hi + vec2(1000.0))
    while out.len() % 64 != 0 {
        out.push(GRID.centre(N - 1))
    }
    out
}

pub struct State {
    points: GpuBuffer<vec2>,
    out: GpuBuffer<vec4>,
    pending: Option<Pending<vec4>>,
    got: Vec<vec4>,
}

pub fn init() -> State {
    let ps = points()
    var points: GpuBuffer<vec2> = buffer(ps.len())
    write(mut points, 0, ps)
    State { points, out: buffer(ps.len()), pending: None, got: Vec::new() }
}

pub fn frame(s: mut State, time: f32, width: u32, height: u32) {
    if s.got.len() > 0 {
        return
    }
    match mut s.pending {
        Some(p) => {
            if let Some(Ok(v)) = p.poll() {
                s.got = take v
            }
        },
        None => {
            dispatch(sample.bind(held(), s.points, mut s.out), over: s.points.len())
            s.pending = Some(read(s.out.items()))
        },
    }
}

@test(frames: 6, gpu: true)
fn the_gpu_reads_the_relief_as_the_cpu_does(s: State) {
    let ps = points()
    assert(s.got.len() == ps.len(), "the samples came back")
    let h = held()
    for i in 0..ps.len() {
        let p = ps[i]
        let c = GRID.cell_at(p)
        let g = s.got[i]
        assert(abs(g.x - h.relief.rise_at(p)) < 1.0e-3, f"the rise at ({p.x}, {p.y})")
        assert(abs(g.y - h.relief.wet_at(p)) < 1.0e-5, f"the wetness at ({p.x}, {p.y})")
        assert(g.z == h.relief.rise(c), f"cell {c}'s rise")
        assert(abs(g.w - h.relief.wet(c)) < 1.0e-6, f"cell {c}'s wetness")
    }
}
"#,
    );
    assert_eq!(super::tests_pass(&dir), 1);
}
