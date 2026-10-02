// first-light's compute pipeline: derives palette entries 4.. from the four seed colours, adding
// `step` once per group of four (wrapping u32 arithmetic, so a host can check it exactly).

struct Params {
    step: u32,
    count: u32,
    _pad: vec2u,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read_write> palette: array<u32>;

@compute @workgroup_size(64)
fn fill(@builtin(global_invocation_id) id: vec3u) {
    let i = id.x;
    if i >= 4u && i < params.count {
        palette[i] = palette[i % 4u] + (i / 4u) * params.step;
    }
}
