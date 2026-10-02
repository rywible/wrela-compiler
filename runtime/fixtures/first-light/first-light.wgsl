// first-light's render pipeline: one full-screen triangle; each pixel is coloured by its distance
// to a circle whose centre the CPU computes each frame. Colours come from the palette that the
// `fill` compute pipeline derived on the first frame.

struct Frame {
    resolution: vec2f,
    center: vec2f,
    time: f32,
    radius: f32,
    _pad: vec2f,
}

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<storage, read> palette: array<u32>;

// Vertex pulling: vertices 0, 1, 2 at (-1, -1), (3, -1), (-1, 3) cover the screen.
@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let x = f32(i32(i & 1u) * 4 - 1);
    let y = f32(i32(i >> 1u) * 4 - 1);
    return vec4f(x, y, 0.0, 1.0);
}

fn colour(i: u32) -> vec3f {
    return unpack4x8unorm(palette[i]).rgb;
}

@fragment
fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let p = pos.xy;
    var c = mix(colour(0u), colour(2u), p.y / frame.resolution.y);
    // Signed distance to the circle, in pixels: negative inside.
    let d = distance(p, frame.center) - frame.radius;
    c += colour(7u) * 0.4 * exp(-max(d, 0.0) / (0.3 * frame.radius));
    let pulse = 0.85 + 0.15 * cos(frame.time * 3.0);
    c = mix(colour(5u) * pulse, c, smoothstep(-1.0, 1.0, d));
    return vec4f(min(c, vec3f(1.0)), 1.0);
}
